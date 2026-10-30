/** GPU renderer for the live preview. Receives the raw frame messages built by
 *  recorder-core/src/preview.rs and converts YUV to RGB in a fragment shader, so the CPU never
 *  touches pixels (the same approach as OBS' camera sources: upload YUV, convert on the GPU).
 *
 *  Message: u32 width, u32 height, u32 format, u64 unix_ms (little endian), then pixels.
 *  format 0 = planar 4:2:0 (Y, U, V), 1 = UYVY, 2 = YUYV, 3 = NV12. */

export const HEADER_LEN = 20;
export const enum Fmt {
  Planar = 0,
  Uyvy = 1,
  Yuyv = 2,
  Nv12 = 3,
}

const VERT = `#version 300 es
out vec2 vUv;
void main() {
  // one big triangle covering the screen; uv (0,0) is the top-left of the image
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  vUv = vec2(p.x, 1.0 - p.y);
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

const COMMON = `#version 300 es
precision highp float;
precision highp int;
in vec2 vUv;
out vec4 outColor;
uniform bool uHD;
vec3 toRgb(float y, float u, float v) {
  // limited ("TV") range, which cameras and capture cards deliver
  y = (y - 16.0 / 255.0) * (255.0 / 219.0);
  u = (u - 0.5) * (255.0 / 224.0);
  v = (v - 0.5) * (255.0 / 224.0);
  vec3 c = uHD ? vec3(y + 1.5748 * v, y - 0.1873 * u - 0.4681 * v, y + 1.8556 * u)
               : vec3(y + 1.402 * v, y - 0.3441 * u - 0.7141 * v, y + 1.772 * u);
  return clamp(c, 0.0, 1.0);
}
`;

const FRAG_PLANAR = `${COMMON}
uniform sampler2D uY; uniform sampler2D uU; uniform sampler2D uV;
void main() {
  outColor = vec4(toRgb(texture(uY, vUv).r, texture(uU, vUv).r, texture(uV, vUv).r), 1.0);
}`;

const FRAG_NV12 = `${COMMON}
uniform sampler2D uY; uniform sampler2D uUV;
void main() {
  vec2 uv = texture(uUV, vUv).rg;
  outColor = vec4(toRgb(texture(uY, vUv).r, uv.r, uv.g), 1.0);
}`;

// Packed 4:2:2 is uploaded as RGBA8 with half the width: one texel holds two pixels.
const FRAG_PACKED = `${COMMON}
uniform sampler2D uP; uniform ivec2 uSize; uniform bool uUyvy;
void main() {
  ivec2 p = ivec2(vUv * vec2(uSize));
  p = clamp(p, ivec2(0), uSize - 1);
  vec4 t = texelFetch(uP, ivec2(p.x >> 1, p.y), 0);
  bool odd = (p.x & 1) == 1;
  float y; float u; float v;
  if (uUyvy) { u = t.r; y = odd ? t.a : t.g; v = t.b; }
  else       { y = odd ? t.b : t.r; u = t.g; v = t.a; }
  outColor = vec4(toRgb(y, u, v), 1.0);
}`;

interface Prog {
  p: WebGLProgram;
  u: Record<string, WebGLUniformLocation | null>;
}

export class YuvRenderer {
  readonly gl: WebGL2RenderingContext;
  private progs: Partial<Record<Fmt, Prog>> = {};
  private tex: WebGLTexture[] = [];
  private texKey = "";
  private vao: WebGLVertexArrayObject;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: false, desynchronized: true });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    this.vao = gl.createVertexArray()!;
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  }

  private prog(fmt: Fmt): Prog {
    const hit = this.progs[fmt];
    if (hit) return hit;
    const gl = this.gl;
    const frag = fmt === Fmt.Planar ? FRAG_PLANAR : fmt === Fmt.Nv12 ? FRAG_NV12 : FRAG_PACKED;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader error");
      return s;
    };
    const p = gl.createProgram()!;
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VERT));
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, frag));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? "link error");
    const names = ["uHD", "uY", "uU", "uV", "uUV", "uP", "uSize", "uUyvy"];
    const u: Prog["u"] = {};
    for (const n of names) u[n] = gl.getUniformLocation(p, n);
    return (this.progs[fmt] = { p, u });
  }

  private ensureTextures(fmt: Fmt, w: number, h: number) {
    const key = `${fmt}:${w}x${h}`;
    if (key === this.texKey) return;
    const gl = this.gl;
    this.tex.forEach((t) => gl.deleteTexture(t));
    this.tex = [];
    const cw = (w + 1) >> 1;
    const ch = (h + 1) >> 1;
    const make = (internal: number, tw: number, th: number, filter: number) => {
      const t = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texStorage2D(gl.TEXTURE_2D, 1, internal, tw, th);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      this.tex.push(t);
    };
    if (fmt === Fmt.Planar) {
      make(gl.R8, w, h, gl.LINEAR);
      make(gl.R8, cw, ch, gl.LINEAR);
      make(gl.R8, cw, ch, gl.LINEAR);
    } else if (fmt === Fmt.Nv12) {
      make(gl.R8, w, h, gl.LINEAR);
      make(gl.RG8, cw, ch, gl.LINEAR);
    } else {
      make(gl.RGBA8, w >> 1, h, gl.NEAREST);
    }
    this.texKey = key;
  }

  private upload(unit: number, format: number, w: number, h: number, data: Uint8Array) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, this.tex[unit]);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, format, gl.UNSIGNED_BYTE, data);
  }

  /** Draw one frame message. Returns its width, height and the sender's timestamp (ms). */
  draw(msg: ArrayBuffer | Uint8Array): { w: number; h: number; sentMs: number } | null {
    const bytes = msg instanceof Uint8Array ? msg : new Uint8Array(msg);
    if (bytes.length < HEADER_LEN) return null;
    const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const w = dv.getUint32(0, true);
    const h = dv.getUint32(4, true);
    const fmt = dv.getUint32(8, true) as Fmt;
    const sentMs = Number(dv.getBigUint64(12, true));
    if (!w || !h) return null;
    const px = bytes.subarray(HEADER_LEN);
    const gl = this.gl;
    const cw = (w + 1) >> 1;
    const ch = (h + 1) >> 1;

    const need = fmt === Fmt.Planar ? w * h + 2 * cw * ch : fmt === Fmt.Nv12 ? w * h + 2 * cw * ch : w * h * 2;
    if (px.length < need) return null;
    if (fmt !== Fmt.Planar && fmt !== Fmt.Nv12 && (w & 1)) return null; // packed 4:2:2 needs an even width

    this.ensureTextures(fmt, w, h);
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    const prog = this.prog(fmt);
    gl.useProgram(prog.p);
    gl.bindVertexArray(this.vao);
    gl.viewport(0, 0, w, h);
    gl.uniform1i(prog.u.uHD, h >= 720 ? 1 : 0);

    if (fmt === Fmt.Planar) {
      this.upload(0, gl.RED, w, h, px.subarray(0, w * h));
      this.upload(1, gl.RED, cw, ch, px.subarray(w * h, w * h + cw * ch));
      this.upload(2, gl.RED, cw, ch, px.subarray(w * h + cw * ch, w * h + 2 * cw * ch));
      gl.uniform1i(prog.u.uY, 0);
      gl.uniform1i(prog.u.uU, 1);
      gl.uniform1i(prog.u.uV, 2);
    } else if (fmt === Fmt.Nv12) {
      this.upload(0, gl.RED, w, h, px.subarray(0, w * h));
      this.upload(1, gl.RG, cw, ch, px.subarray(w * h, w * h + 2 * cw * ch));
      gl.uniform1i(prog.u.uY, 0);
      gl.uniform1i(prog.u.uUV, 1);
    } else {
      this.upload(0, gl.RGBA, w >> 1, h, px.subarray(0, w * h * 2));
      gl.uniform1i(prog.u.uP, 0);
      gl.uniform2i(prog.u.uSize, w, h);
      gl.uniform1i(prog.u.uUyvy, fmt === Fmt.Uyvy ? 1 : 0);
    }
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    return { w, h, sentMs };
  }

  dispose() {
    const gl = this.gl;
    this.tex.forEach((t) => gl.deleteTexture(t));
    Object.values(this.progs).forEach((p) => p && gl.deleteProgram(p.p));
    gl.deleteVertexArray(this.vao);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
  }
}

/** Build a frame message (used by the self-test). */
export function makeMessage(w: number, h: number, fmt: Fmt, pixels: number[]): Uint8Array {
  const out = new Uint8Array(HEADER_LEN + pixels.length);
  const dv = new DataView(out.buffer);
  dv.setUint32(0, w, true);
  dv.setUint32(4, h, true);
  dv.setUint32(8, fmt, true);
  dv.setBigUint64(12, BigInt(Date.now()), true);
  out.set(pixels, HEADER_LEN);
  return out;
}

/** Colour-checks every shader path with known frames; returns one line per case. Run in the real
 *  webview (see `XSPECTOR_DEBUG`), because GPU drivers differ. */
export function selfTest(): string[] {
  // BT.601 limited-range YUV for pure colours (frames here are < 720 px high).
  const colours: Record<string, { y: number; u: number; v: number; rgb: [number, number, number] }> = {
    red: { y: 81, u: 90, v: 240, rgb: [255, 0, 0] },
    green: { y: 145, u: 54, v: 34, rgb: [0, 255, 0] },
    blue: { y: 41, u: 240, v: 110, rgb: [0, 0, 255] },
    white: { y: 235, u: 128, v: 128, rgb: [255, 255, 255] },
    black: { y: 16, u: 128, v: 128, rgb: [0, 0, 0] },
  };
  const lines: string[] = [];
  const canvas = document.createElement("canvas");
  let r: YuvRenderer;
  try {
    r = new YuvRenderer(canvas);
  } catch (e) {
    return [`selftest: ${String(e)}`];
  }
  for (const [name, c] of Object.entries(colours)) {
    const cases: [string, Uint8Array][] = [
      ["planar", makeMessage(2, 2, Fmt.Planar, [c.y, c.y, c.y, c.y, c.u, c.v])],
      ["nv12", makeMessage(2, 2, Fmt.Nv12, [c.y, c.y, c.y, c.y, c.u, c.v])],
      ["uyvy", makeMessage(2, 1, Fmt.Uyvy, [c.u, c.y, c.v, c.y])],
      ["yuyv", makeMessage(2, 1, Fmt.Yuyv, [c.y, c.u, c.y, c.v])],
    ];
    for (const [fmt, msg] of cases) {
      r.draw(msg);
      const px = new Uint8Array(4);
      r.gl.readPixels(0, 0, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
      const ok = c.rgb.every((v, i) => Math.abs(v - px[i]) <= 3);
      lines.push(`selftest ${fmt.padEnd(6)} ${name.padEnd(5)} -> rgb(${px[0]},${px[1]},${px[2]}) expected rgb(${c.rgb.join(",")}) ${ok ? "OK" : "MISMATCH"}`);
    }
  }
  r.dispose();
  return lines;
}
