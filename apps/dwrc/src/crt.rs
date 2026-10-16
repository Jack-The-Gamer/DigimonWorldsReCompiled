//! The CRT filter: draws the game's picture the way a consumer CRT
//! television showed it, as an OpenGL shader run inside egui's frame through
//! a paint callback.
//!
//! * **Scanlines.** Each line of the console's picture is a horizontal beam
//!   with a Gaussian profile whose width grows with brightness, so dark
//!   areas show clear gaps between lines and bright ones bloom together.
//!   Brightness is preserved (each beam is normalised by its width). 480-line
//!   interlaced modes are drawn as 240 beams, as a television showed a field.
//! * **Phosphor mask.** An aperture grille: every screen pixel column favours
//!   red, green or blue in turn.
//! * **Curvature** (optional): a slight barrel distortion with darker edges
//!   and rounded-off corners.
//!
//! The math runs in linear light. The frame is uploaded to the filter's own
//! RGBA8 texture (gamma-encoded, linear filtering), and the shader samples
//! source pixels at their centres, so it never depends on how egui stores
//! its own textures.

use eframe::glow::{self, HasContext};
use std::sync::Mutex;

/// The latest picture from the emulator.
#[derive(Default)]
pub struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Effect strengths, 0..1 (curvature: 0 = flat).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub scanlines: f32,
    pub mask: f32,
    pub curvature: f32,
}

struct Locs {
    tex: Option<glow::UniformLocation>,
    src: Option<glow::UniformLocation>,
    lines: Option<glow::UniformLocation>,
    scan: Option<glow::UniformLocation>,
    mask: Option<glow::UniformLocation>,
    curve: Option<glow::UniformLocation>,
}

pub struct Crt {
    program: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
    locs: Locs,
    /// (seq, width, height) of what is in the texture.
    uploaded: Mutex<(u64, u32, u32)>,
    pub frame: Mutex<Frame>,
}

const VS: &str = r#"
out vec2 v_uv;
void main() {
    // One triangle covering the viewport (egui sets it to the picture's rect).
    vec2 p = vec2(gl_VertexID == 1 ? 3.0 : -1.0, gl_VertexID == 2 ? 3.0 : -1.0);
    v_uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    gl_Position = vec4(p, 0.0, 1.0);
}
"#;

const FS: &str = r#"
uniform sampler2D u_tex;
uniform vec2 u_src;     // source size in pixels
uniform float u_lines;  // scanlines drawn (the source's lines, or half of them for 480i)
uniform float u_scan;   // scanline strength 0..1
uniform float u_mask;   // phosphor mask strength 0..0.5
uniform float u_curve;  // barrel curvature, 0 = flat
in vec2 v_uv;
out vec4 o_color;

// A source pixel (x, y in pixel units, centres at .5), in linear light.
vec3 px(float x, float y) {
    vec2 t = clamp(vec2(x, y), vec2(0.5), u_src - vec2(0.5));
    return pow(texture(u_tex, t / u_src).rgb, vec3(2.2));
}

// One line at horizontal position x: neighbouring pixels blend over a
// narrow band, so edges stay crisp without stair-stepping.
vec3 line_at(float x, float y) {
    float fx = x - 0.5;
    float x0 = floor(fx);
    float f = smoothstep(0.2, 0.8, fx - x0);
    return mix(px(x0 + 0.5, y), px(x0 + 1.5, y), f);
}

// A beam of colour c seen at distance d (in lines) from its centre.
vec3 beam(vec3 c, float d) {
    float l = max(c.r, max(c.g, c.b));
    float s = mix(0.20, 0.34, sqrt(l));
    return c * (exp(-0.5 * d * d / (s * s)) / (s * 2.5066));
}

void main() {
    vec2 uv = v_uv;
    if (u_curve > 0.0) {
        vec2 c = uv * 2.0 - 1.0;
        c *= 1.0 + u_curve * c.yx * c.yx;
        uv = c * 0.5 + 0.5;
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            o_color = vec4(0.0, 0.0, 0.0, 1.0);
            return;
        }
    }
    float x = uv.x * u_src.x;
    vec3 flat_col = line_at(x, floor(uv.y * u_src.y) + 0.5);
    float rows = u_src.y / u_lines;
    float ly = uv.y * u_lines - 0.5;
    float l0 = floor(ly);
    float fy = ly - l0;
    vec3 a = line_at(x, (l0 + 0.5) * rows);
    vec3 b = line_at(x, (l0 + 1.5) * rows);
    vec3 scan = beam(a, fy) + beam(b, 1.0 - fy);
    vec3 col = mix(flat_col, scan, u_scan);
    float m = mod(floor(gl_FragCoord.x), 3.0);
    vec3 mask = vec3(1.0 - u_mask);
    if (m < 0.5) mask.r = 1.0; else if (m < 1.5) mask.g = 1.0; else mask.b = 1.0;
    col *= mask / (1.0 - u_mask * 2.0 / 3.0);
    if (u_curve > 0.0) {
        vec2 v = uv * (1.0 - uv);
        col *= pow(clamp(16.0 * v.x * v.y, 0.0, 1.0), 0.15);
    }
    o_color = vec4(pow(clamp(col, 0.0, 1.0), vec3(1.0 / 2.2)), 1.0);
}
"#;

/// The GLSL version line for this context (`None` below OpenGL 3 / ES 3).
fn header(gl: &glow::Context) -> Option<&'static str> {
    let v = gl.version();
    if v.is_embedded {
        (v.major >= 3).then_some("#version 300 es\nprecision highp float;\n")
    } else if (v.major, v.minor) >= (3, 3) {
        Some("#version 330\n")
    } else if (v.major, v.minor) >= (3, 2) {
        Some("#version 150\n")
    } else if v.major >= 3 {
        Some("#version 130\n")
    } else {
        None
    }
}

impl Crt {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let header = header(gl).ok_or_else(|| format!("needs OpenGL 3 (this is {}.{})", gl.version().major, gl.version().minor))?;
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
                let s = gl.create_shader(kind)?;
                gl.shader_source(s, &format!("{header}{src}"));
                gl.compile_shader(s);
                if !gl.get_shader_compile_status(s) {
                    let log = gl.get_shader_info_log(s);
                    gl.delete_shader(s);
                    gl.delete_program(program);
                    return Err(format!("shader: {log}"));
                }
                gl.attach_shader(program, s);
                shaders.push(s);
            }
            gl.link_program(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("link: {log}"));
            }
            let vao = gl.create_vertex_array()?;
            let texture = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.bind_texture(glow::TEXTURE_2D, None);
            let loc = |n: &str| gl.get_uniform_location(program, n);
            let locs = Locs { tex: loc("u_tex"), src: loc("u_src"), lines: loc("u_lines"), scan: loc("u_scan"), mask: loc("u_mask"), curve: loc("u_curve") };
            Ok(Self { program, vao, texture, locs, uploaded: Mutex::new((u64::MAX, 0, 0)), frame: Mutex::new(Frame::default()) })
        }
    }

    /// Hands the filter a new picture (RGBA, gamma-encoded).
    pub fn set_frame(&self, seq: u64, width: u32, height: u32, rgba: &[u8]) {
        let mut f = self.frame.lock().unwrap();
        f.seq = seq;
        f.width = width;
        f.height = height;
        f.rgba.clear();
        f.rgba.extend_from_slice(rgba);
    }

    /// Draws the latest picture into the current viewport (call from an
    /// `egui_glow::CallbackFn`; egui restores its own GL state afterwards).
    pub fn paint(&self, gl: &glow::Context, p: Params) {
        let f = self.frame.lock().unwrap();
        if f.width == 0 || f.height == 0 || f.rgba.len() < (f.width * f.height * 4) as usize {
            return;
        }
        let (w, h) = (f.width as i32, f.height as i32);
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            let mut up = self.uploaded.lock().unwrap();
            if up.0 != f.seq || (up.1, up.2) != (f.width, f.height) {
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                let data = glow::PixelUnpackData::Slice(Some(&f.rgba));
                if (up.1, up.2) == (f.width, f.height) {
                    gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, w, h, glow::RGBA, glow::UNSIGNED_BYTE, data);
                } else {
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, data);
                }
                *up = (f.seq, f.width, f.height);
            }
            let lines = if f.height > 288 { f.height as f32 / 2.0 } else { f.height as f32 };
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.uniform_1_i32(self.locs.tex.as_ref(), 0);
            gl.uniform_2_f32(self.locs.src.as_ref(), w as f32, h as f32);
            gl.uniform_1_f32(self.locs.lines.as_ref(), lines);
            gl.uniform_1_f32(self.locs.scan.as_ref(), p.scanlines.clamp(0.0, 1.0));
            gl.uniform_1_f32(self.locs.mask.as_ref(), p.mask.clamp(0.0, 1.0) * 0.45);
            gl.uniform_1_f32(self.locs.curve.as_ref(), p.curvature.clamp(0.0, 1.0) * 0.06);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }
}
