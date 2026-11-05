//! The CRT filter: draws the game's picture the way a consumer CRT
//! television showed it, as OpenGL shaders run inside egui's frame through
//! a paint callback.
//!
//! * **Scanlines.** Each line of the console's picture is a horizontal beam
//!   with a Gaussian profile. Every beam keeps its energy (its colour's worth
//!   of light per line) and its spot widens with brightness, per colour
//!   channel, so dark areas show clear gaps between lines and bright ones
//!   merge. A spot can't be brighter than full white at its centre: the
//!   brightest colours saturate instead of clipping, so their hue is kept.
//!   480-line interlaced pictures are drawn as both fields at once (480 beams
//!   at half spacing), so their full detail stays. The beams are smoothed by
//!   the size of a screen pixel, so a small window shows a calmer picture
//!   rather than aliased lines.
//! * **Horizontally** the beam follows the signal through a narrow Gaussian
//!   that widens a little on bright pixels (they bleed into dark ones). Spot
//!   sizes are in units of the screen's line height, so 320, 512 and 640
//!   pixel modes, widescreen and stretched output all keep the same tube.
//! * **Phosphor mask.** An aperture grille whose period follows the
//!   picture's size on screen (about 480 triads across a 4:3 picture):
//!   magenta/green pairs below ~900 lines, RGB stripes at 1080, wider ones
//!   above. The pattern is box-filtered into whole screen pixels and keeps
//!   the average brightness.
//! * **Glow.** A share of the light spreads through the glass: the picture
//!   is blurred (two passes at the console's resolution, into textures of
//!   our own) and mixed in, keeping the total brightness.
//! * **Curvature** (optional): a slight barrel distortion with darker edges
//!   and rounded, anti-aliased corners.
//!
//! The math runs in linear light. The frame is uploaded to the filter's own
//! RGBA8 texture (gamma-encoded) and the shaders read whole texels, so they
//! never depend on how egui stores its own textures.

use eframe::{egui, egui_glow};
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
    pub glow: f32,
    pub curvature: f32,
}

/// The glow's blur, in scanlines (sigma).
const GLOW_SIZE: f32 = 1.5;
/// The share of light the Glow slider spreads at most.
const GLOW_MAX: f32 = 0.3;

struct Locs {
    tex: Option<glow::UniformLocation>,
    halo: Option<glow::UniformLocation>,
    src: Option<glow::UniformLocation>,
    out: Option<glow::UniformLocation>,
    field: Option<glow::UniformLocation>,
    sx: Option<glow::UniformLocation>,
    spot: Option<glow::UniformLocation>,
    mask: Option<glow::UniformLocation>,
    period: Option<glow::UniformLocation>,
    glow: Option<glow::UniformLocation>,
    curve: Option<glow::UniformLocation>,
}

struct BlurLocs {
    tex: Option<glow::UniformLocation>,
    dir: Option<glow::UniformLocation>,
    sigma: Option<glow::UniformLocation>,
}

/// What the textures hold.
struct State {
    /// (seq, width, height) of the picture in `texture`.
    uploaded: (u64, u32, u32),
    /// (seq, horizontal blur) of the glow in `glow[1]`.
    glowed: Option<(u64, u32)>,
}

pub struct Crt {
    program: glow::Program,
    blur: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
    /// The glow's horizontal pass, then its result.
    glow: [glow::Texture; 2],
    fbo: glow::Framebuffer,
    locs: Locs,
    blur_locs: BlurLocs,
    state: Mutex<State>,
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
uniform sampler2D u_tex;   // the console's picture (gamma-encoded)
uniform sampler2D u_halo;  // the same picture blurred (gamma-encoded)
uniform vec2 u_src;     // source size in pixels
uniform vec2 u_out;     // picture size on screen in pixels
uniform float u_field;  // source rows per scanline: 1, or 2 for 480i (both fields at once)
uniform float u_sx;     // source pixels per scanline height, horizontally
uniform vec2 u_spot;    // beam spot size (sigma, in scanlines) of dark and of full-bright colour
uniform float u_mask;   // phosphor mask strength 0..0.45
uniform float u_period; // mask period in screen pixels (2 = magenta/green)
uniform float u_glow;   // share of light diffused by the glass, 0..
uniform float u_curve;  // barrel curvature, 0 = flat
in vec2 v_uv;
out vec4 o_color;

// A source pixel in linear light.
vec3 px(float x, float y) {
    ivec2 t = ivec2(clamp(vec2(x, y), vec2(0.0), u_src - vec2(1.0)));
    return pow(texelFetch(u_tex, t, 0).rgb, vec3(2.2));
}

// The beam's signal along source row y at horizontal position x (pixel
// units): a Gaussian over the nearest pixels whose width grows with their
// brightness, so bright pixels bleed a little into dark ones.
vec3 row_at(float x, float y) {
    float fx = x - 0.5;
    float x0 = floor(fx);
    float f = fx - x0;
    vec3 a = px(x0 - 1.0, y), b = px(x0, y), c = px(x0 + 1.0, y), d = px(x0 + 2.0, y);
    vec3 s = mix(vec3(0.18), vec3(0.32), sqrt(max(b, c))) * u_sx;
    vec3 k = -0.5 / (s * s);
    vec3 wa = exp(k * (1.0 + f) * (1.0 + f)), wb = exp(k * f * f);
    vec3 wc = exp(k * (1.0 - f) * (1.0 - f)), wd = exp(k * (2.0 - f) * (2.0 - f));
    return (a * wa + b * wb + c * wc + d * wd) / (wa + wb + wc + wd);
}

// Phosphor mask at screen column x: an aperture grille, every stripe a third
// of the period (box-filtered into whole pixels), or magenta/green pairs when
// only two pixels are available. Average brightness is kept.
vec3 mask_at(float x) {
    vec3 cov;
    float mean;
    if (u_period < 2.5) {
        cov = mod(floor(x), 2.0) < 0.5 ? vec3(1.0, 0.0, 1.0) : vec3(0.0, 1.0, 0.0);
        mean = 0.5;
    } else {
        float i = mod(floor(x), u_period);
        vec3 lo = vec3(0.0, 1.0, 2.0) * (u_period / 3.0);
        cov = clamp(min(lo + u_period / 3.0, vec3(i + 1.0)) - max(lo, vec3(i)), 0.0, 1.0);
        mean = 1.0 / 3.0;
    }
    return (1.0 - u_mask * (1.0 - cov)) / (1.0 - u_mask * (1.0 - mean));
}

void main() {
    vec2 uv = v_uv;
    float edge = 1.0;
    if (u_curve > 0.0) {
        vec2 c = uv * 2.0 - 1.0;
        c *= 1.0 + u_curve * c.yx * c.yx;
        uv = c * 0.5 + 0.5;
        // Distance to the tube's edge in screen pixels, with rounded corners,
        // faded over one pixel; and slightly darker edges.
        vec2 e = min(uv, 1.0 - uv) * u_out;
        float r = u_out.y * u_curve * 0.5;
        float dist = (e.x < r && e.y < r) ? r - length(vec2(r) - e) : min(e.x, e.y);
        edge = clamp(dist + 0.5, 0.0, 1.0);
        vec2 v = clamp(uv * (1.0 - uv), 0.0, 1.0);
        edge *= pow(clamp(16.0 * v.x * v.y, 0.0, 1.0), 0.15);
    }
    float x = uv.x * u_src.x;
    float y = uv.y * u_src.y - 0.5;
    float y0 = floor(y);
    float fy = y - y0;
    // A screen pixel covers this many source rows: the beams are smoothed by
    // that much, so small pictures don't alias.
    float foot = u_src.y / u_out.y;
    vec3 col = vec3(0.0);
    for (int i = -1; i <= 2; i++) {
        vec3 c = row_at(x, y0 + float(i));
        // Each beam keeps its energy (one row's worth of its colour), spread
        // over a spot that widens with brightness; a spot can't be brighter
        // than full at its centre, so the brightest colours saturate.
        vec3 s = clamp(c * 0.3989, vec3(u_spot.x), vec3(u_spot.y)) * u_field;
        vec3 e = min(c, s * 2.5066);
        vec3 se = min(sqrt(s * s + foot * foot / 12.0), vec3(0.6));
        float d = float(i) - fy;
        col += e * exp(-0.5 * d * d / (se * se)) / (se * 2.5066);
    }
    col *= mask_at(gl_FragCoord.x);
    col = mix(col, pow(texture(u_halo, uv).rgb, vec3(2.2)), u_glow);
    col *= edge;
    o_color = vec4(pow(clamp(col, 0.0, 1.0), vec3(1.0 / 2.2)), 1.0);
}
"#;

/// One direction of the glow's blur, at the source's resolution.
const BLUR_FS: &str = r#"
uniform sampler2D u_tex;  // gamma-encoded, source size
uniform vec2 u_dir;       // (1, 0) or (0, 1)
uniform float u_sigma;    // in source pixels along u_dir
out vec4 o_color;

void main() {
    ivec2 size = textureSize(u_tex, 0);
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 dir = ivec2(u_dir);
    vec3 sum = vec3(0.0);
    float wsum = 0.0;
    for (int i = -7; i <= 7; i++) {
        float w = exp(-0.5 * float(i * i) / (u_sigma * u_sigma));
        ivec2 t = clamp(p + dir * i, ivec2(0), size - ivec2(1));
        sum += w * pow(texelFetch(u_tex, t, 0).rgb, vec3(2.2));
        wsum += w;
    }
    o_color = vec4(pow(sum / wsum, vec3(1.0 / 2.2)), 1.0);
}
"#;

/// The GLSL version line for this context (`None` below OpenGL 3 / ES 3).
fn header(gl: &glow::Context) -> Option<&'static str> {
    let v = gl.version();
    if v.is_embedded {
        (v.major >= 3).then_some("#version 300 es\nprecision highp float;\nprecision highp int;\n")
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

unsafe fn link(gl: &glow::Context, header: &str, fs: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, &format!("{header}{src}"));
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                let log = gl.get_shader_info_log(s);
                gl.delete_shader(s);
                for s in shaders {
                    gl.delete_shader(s);
                }
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
        Ok(program)
    }
}

unsafe fn new_texture(gl: &glow::Context) -> Result<glow::Texture, String> {
    unsafe {
        let t = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(t)
    }
}

impl Crt {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let header = header(gl).ok_or_else(|| format!("needs OpenGL 3 (this is {}.{})", gl.version().major, gl.version().minor))?;
        unsafe {
            let program = link(gl, header, FS)?;
            let blur = link(gl, header, BLUR_FS).inspect_err(|_| gl.delete_program(program))?;
            let vao = gl.create_vertex_array()?;
            let texture = new_texture(gl)?;
            let glow = [new_texture(gl)?, new_texture(gl)?];
            let fbo = gl.create_framebuffer()?;
            let loc = |p, n: &str| gl.get_uniform_location(p, n);
            let locs = Locs {
                tex: loc(program, "u_tex"),
                halo: loc(program, "u_halo"),
                src: loc(program, "u_src"),
                out: loc(program, "u_out"),
                field: loc(program, "u_field"),
                sx: loc(program, "u_sx"),
                spot: loc(program, "u_spot"),
                mask: loc(program, "u_mask"),
                period: loc(program, "u_period"),
                glow: loc(program, "u_glow"),
                curve: loc(program, "u_curve"),
            };
            let blur_locs = BlurLocs { tex: loc(blur, "u_tex"), dir: loc(blur, "u_dir"), sigma: loc(blur, "u_sigma") };
            let state = Mutex::new(State { uploaded: (u64::MAX, 0, 0), glowed: None });
            Ok(Self { program, blur, vao, texture, glow, fbo, locs, blur_locs, state, frame: Mutex::new(Frame::default()) })
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

    /// Draws the latest picture into the callback's viewport (call from an
    /// `egui_glow::CallbackFn`; egui restores its own GL state afterwards).
    pub fn paint(&self, painter: &egui_glow::Painter, info: &egui::PaintCallbackInfo, p: Params) {
        let gl = painter.gl();
        let f = self.frame.lock().unwrap();
        if f.width == 0 || f.height == 0 || f.rgba.len() < (f.width * f.height * 4) as usize {
            return;
        }
        let (w, h) = (f.width as i32, f.height as i32);
        let vp = info.viewport_in_pixels();
        let (ow, oh) = (vp.width_px.max(1) as f32, vp.height_px.max(1) as f32);
        // 480-line pictures are interlaced: two rows per scanline.
        let field = if f.height > 288 { 2.0 } else { 1.0 };
        // Source pixels per scanline height, horizontally (1 for 320 pixels
        // at 4:3, 2 for 640).
        let sx = (oh * field / h as f32) / (ow / w as f32);
        let s = p.scanlines.clamp(0.0, 1.0);
        let glow = p.glow.clamp(0.0, 1.0) * GLOW_MAX;
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            let mut st = self.state.lock().unwrap();
            let resized = (st.uploaded.1, st.uploaded.2) != (f.width, f.height);
            if st.uploaded.0 != f.seq || resized {
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                let data = glow::PixelUnpackData::Slice(Some(&f.rgba));
                if resized {
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, data);
                    for t in self.glow {
                        gl.bind_texture(glow::TEXTURE_2D, Some(t));
                        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                    }
                    st.glowed = None;
                } else {
                    gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, w, h, glow::RGBA, glow::UNSIGNED_BYTE, data);
                }
                st.uploaded = (f.seq, f.width, f.height);
            }
            // The glow: blur the picture into glow[0] across, then glow[1] down.
            let key = (f.seq, (GLOW_SIZE * sx).to_bits());
            let mut glow_ok = glow > 0.0;
            if glow_ok && st.glowed != Some(key) {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                gl.viewport(0, 0, w, h);
                gl.use_program(Some(self.blur));
                gl.uniform_1_i32(self.blur_locs.tex.as_ref(), 0);
                gl.bind_vertex_array(Some(self.vao));
                for (from, to, dir, sigma) in [(self.texture, self.glow[0], (1.0, 0.0), GLOW_SIZE * sx), (self.glow[0], self.glow[1], (0.0, 1.0), GLOW_SIZE * field)] {
                    gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(to), 0);
                    if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                        glow_ok = false;
                        break;
                    }
                    gl.bind_texture(glow::TEXTURE_2D, Some(from));
                    gl.uniform_2_f32(self.blur_locs.dir.as_ref(), dir.0, dir.1);
                    gl.uniform_1_f32(self.blur_locs.sigma.as_ref(), sigma);
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, None, 0);
                gl.bind_framebuffer(glow::FRAMEBUFFER, painter.intermediate_fbo());
                gl.viewport(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
                gl.enable(glow::SCISSOR_TEST);
                st.glowed = glow_ok.then_some(key);
            }
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.glow[1]));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.uniform_1_i32(self.locs.tex.as_ref(), 0);
            gl.uniform_1_i32(self.locs.halo.as_ref(), 1);
            gl.uniform_2_f32(self.locs.src.as_ref(), w as f32, h as f32);
            gl.uniform_2_f32(self.locs.out.as_ref(), ow, oh);
            gl.uniform_1_f32(self.locs.field.as_ref(), field);
            gl.uniform_1_f32(self.locs.sx.as_ref(), sx);
            // Spot sizes: 0.5 is a flat picture (no scanlines); stronger
            // scanlines shrink the dark spot more than the bright one.
            gl.uniform_2_f32(self.locs.spot.as_ref(), 0.5 - 0.34 * s, 0.5 - 0.18 * s);
            gl.uniform_1_f32(self.locs.mask.as_ref(), p.mask.clamp(0.0, 1.0) * 0.45);
            // About 480 triads across a 4:3 picture: 3 pixels at 1080 lines.
            gl.uniform_1_f32(self.locs.period.as_ref(), (oh / 360.0).round().clamp(2.0, 6.0));
            gl.uniform_1_f32(self.locs.glow.as_ref(), if glow_ok { glow } else { 0.0 });
            gl.uniform_1_f32(self.locs.curve.as_ref(), p.curvature.clamp(0.0, 1.0) * 0.06);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }
}
