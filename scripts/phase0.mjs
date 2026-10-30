#!/usr/bin/env node
// Phase 0 spike: probe devices/formats/encoders and measure CPU/RAM of one FFmpeg recording.
// Usage:
//   node scripts/phase0.mjs devices
//   node scripts/phase0.mjs formats <deviceName|index>
//   node scripts/phase0.mjs encoders
//   node scripts/phase0.mjs record <device> [--size 1280x720] [--fps 30] [--seconds 60]
//        [--encoder auto|h264_videotoolbox|h264_nvenc|h264_qsv|h264_amf|libx264]
//        [--deinterlace] [--out ./out] [--bitrate 4M]
import { spawn, spawnSync } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { join } from 'node:path'

const isWin = process.platform === 'win32'
const isMac = process.platform === 'darwin'
const FFMPEG = process.env.FFMPEG || 'ffmpeg'

const [cmd, ...rest] = process.argv.slice(2)
const flags = {}
const pos = []
for (let i = 0; i < rest.length; i++) {
  if (rest[i].startsWith('--')) {
    const k = rest[i].slice(2)
    const v = rest[i + 1] && !rest[i + 1].startsWith('--') ? rest[++i] : true
    flags[k] = v
  } else pos.push(rest[i])
}

// ffmpeg prints device/format listings to stderr and exits non-zero; that is expected.
const run = (args) => spawnSync(FFMPEG, ['-hide_banner', ...args], { encoding: 'utf8' })
const show = (r) => console.log((r.stderr || '') + (r.stdout || ''))

function inputArgs(device, { size, fps }) {
  if (isMac) {
    // "<video>:none" = video only. Device may be an index or a name.
    const a = ['-f', 'avfoundation', '-framerate', String(fps)]
    if (size) a.push('-video_size', size)
    return [...a, '-i', `${device}:none`]
  }
  const a = ['-f', 'dshow', '-framerate', String(fps)]
  if (size) a.push('-video_size', size)
  return [...a, '-i', `video=${device}`]
}

function probeEncoders() {
  const candidates = isMac
    ? ['h264_videotoolbox', 'libx264']
    : ['h264_nvenc', 'h264_qsv', 'h264_amf', 'libx264']
  const result = []
  for (const enc of candidates) {
    const r = run([
      '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30',
      '-t', '1', '-pix_fmt', 'yuv420p', '-c:v', enc, '-f', 'null', '-',
    ])
    result.push({ encoder: enc, usable: r.status === 0 })
  }
  return result
}

function sample(pid) {
  if (isWin) {
    const r = spawnSync('powershell', [
      '-NoProfile', '-Command',
      `$p=Get-Process -Id ${pid} -ErrorAction SilentlyContinue; if($p){"$($p.CPU) $($p.WorkingSet64/1KB)"}`,
    ], { encoding: 'utf8' })
    const [cpuSec, rssKb] = r.stdout.trim().split(/\s+/).map(Number)
    return Number.isFinite(cpuSec) ? { cpuSec, rssKb } : null
  }
  const r = spawnSync('ps', ['-o', '%cpu=,rss=', '-p', String(pid)], { encoding: 'utf8' })
  const [cpu, rssKb] = r.stdout.trim().split(/\s+/).map(Number)
  return Number.isFinite(cpu) ? { cpu, rssKb } : null
}

async function record(device) {
  const fps = Number(flags.fps || 30)
  const seconds = Number(flags.seconds || 60)
  const out = String(flags.out || './out')
  const bitrate = String(flags.bitrate || '4M')
  mkdirSync(out, { recursive: true })

  let encoder = String(flags.encoder || 'auto')
  if (encoder === 'auto') {
    encoder = probeEncoders().find((e) => e.usable)?.encoder
    if (!encoder) throw new Error('no usable H.264 encoder')
  }
  const encArgs = {
    h264_videotoolbox: ['-c:v', 'h264_videotoolbox', '-b:v', bitrate, '-realtime', '1'],
    h264_nvenc: ['-c:v', 'h264_nvenc', '-preset', 'p4', '-b:v', bitrate],
    h264_qsv: ['-c:v', 'h264_qsv', '-b:v', bitrate],
    h264_amf: ['-c:v', 'h264_amf', '-b:v', bitrate],
    libx264: ['-c:v', 'libx264', '-preset', 'veryfast', '-b:v', bitrate],
  }[encoder]

  const vf = flags.deinterlace ? ['-vf', 'yadif=mode=send_frame'] : []
  const file = join(out, `CH1_${encoder}${flags.deinterlace ? '_yadif' : ''}.mp4`)
  const args = [
    '-hide_banner', '-y',
    ...inputArgs(device, { size: flags.size, fps }),
    '-t', String(seconds),
    ...vf, '-pix_fmt', 'yuv420p', ...encArgs,
    '-g', String(fps * 2), // keyframe every 2s so 30-min segments can cut cleanly later
    '-movflags', '+frag_keyframe+empty_moov+default_base_moof', // survives power loss
    file,
  ]
  console.log(`encoder=${encoder}\n${FFMPEG} ${args.join(' ')}\n`)

  const ff = spawn(FFMPEG, args, { stdio: ['ignore', 'ignore', 'pipe'] })
  let lastStats = ''
  ff.stderr.on('data', (d) => {
    const m = String(d).match(/frame=.*speed=\s*\S+/g)
    if (m) lastStats = m[m.length - 1]
  })

  const samples = []
  let prev = null
  const timer = setInterval(() => {
    const s = sample(ff.pid)
    if (!s) return
    let cpuPct = s.cpu
    if (isWin) {
      cpuPct = prev ? ((s.cpuSec - prev.cpuSec) / 2) * 100 : 0
      prev = s
    }
    samples.push({ cpu: cpuPct, rssMb: s.rssKb / 1024 })
  }, 2000)

  const code = await new Promise((res) => ff.on('close', res))
  clearInterval(timer)

  const cpus = samples.map((s) => s.cpu)
  const rss = samples.map((s) => s.rssMb)
  const avg = (a) => (a.reduce((x, y) => x + y, 0) / (a.length || 1)).toFixed(1)
  console.log(`exit=${code} file=${file}`)
  console.log(`last ffmpeg stats: ${lastStats}`)
  console.log(`CPU% (of one core) avg=${avg(cpus)} max=${Math.max(...cpus, 0).toFixed(1)}`)
  console.log(`RSS MB first=${(rss[0] ?? 0).toFixed(0)} last=${(rss.at(-1) ?? 0).toFixed(0)} max=${Math.max(...rss, 0).toFixed(0)}  (growth = leak suspect)`)
}

switch (cmd) {
  case 'devices':
    show(isMac
      ? run(['-f', 'avfoundation', '-list_devices', 'true', '-i', ''])
      : run(['-list_devices', 'true', '-f', 'dshow', '-i', 'dummy']))
    break
  case 'formats':
    if (!pos[0]) throw new Error('formats needs a device')
    show(isMac
      // an unsupported mode makes avfoundation print "Supported modes"
      ? run(['-f', 'avfoundation', '-framerate', '1', '-video_size', '1x1', '-i', `${pos[0]}:none`])
      : run(['-f', 'dshow', '-list_options', 'true', '-i', `video=${pos[0]}`]))
    break
  case 'encoders':
    console.table(probeEncoders())
    break
  case 'record':
    if (!pos[0]) throw new Error('record needs a device')
    await record(pos[0])
    break
  default:
    console.log('commands: devices | formats <device> | encoders | record <device> [flags]')
}
