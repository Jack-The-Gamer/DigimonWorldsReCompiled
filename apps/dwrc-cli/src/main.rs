//! Developer command-line tool for Digimon Worlds Re:Compiled.

use anyhow::{bail, Context, Result};
use dwrc_disc::{iso9660::IsoFs, Disc, PsxExe, SectorSource};
use dwrc_psx::{cdrom::DiscRegion, Button, Psx, VideoStandard};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") => inspect(PathBuf::from(args.get(1).context("usage: inspect <cue>")?)),
        Some("extract") => {
            let mut disc = Disc::open(&PathBuf::from(&args[1]))?;
            let fs = IsoFs::mount(&mut disc)?;
            let e = fs.find(&mut disc, &args[2])?.context("file not found")?;
            let data = fs.read(&mut disc, &e)?;
            std::fs::write(&args[3], data)?;
            Ok(())
        }
        Some("run") => run(&args[1..]),
        Some("disasm") => disasm(&args[1..]),
        Some("import") => {
            // import <data-root> <folder>
            let paths = dwrc_runtime::Paths::at(args.get(1).context("data root")?);
            paths.ensure()?;
            let mut lib = dwrc_runtime::Library::load(&paths);
            let rep = lib.scan_folder(std::path::Path::new(args.get(2).context("folder")?), &mut |n, f| {
                if f >= 1.0 { println!("verified {n}") }
            });
            lib.save(&paths)?;
            println!("{rep:#?}");
            Ok(())
        }
        _ => bail!(
            "commands:\n  inspect <cue>\n  extract <cue> <path> <out>\n  run <cue> --bios <bin> [--frames N] [--shots dir] [--every K] [--fast] [--input frame:button:len,...]\n  disasm <exe> <addr> <count>"
        ),
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn parse_button(s: &str) -> Option<Button> {
    Some(match s.to_ascii_lowercase().as_str() {
        "start" => Button::Start,
        "select" => Button::Select,
        "x" | "cross" => Button::Cross,
        "o" | "circle" => Button::Circle,
        "t" | "triangle" => Button::Triangle,
        "s" | "square" => Button::Square,
        "up" => Button::Up,
        "down" => Button::Down,
        "left" => Button::Left,
        "right" => Button::Right,
        "l1" => Button::L1,
        "r1" => Button::R1,
        "l2" => Button::L2,
        "r2" => Button::R2,
        _ => return None,
    })
}

fn run(args: &[String]) -> Result<()> {
    let cue = PathBuf::from(args.first().context("missing cue")?);
    let bios_path = opt(args, "--bios").context("--bios required")?;
    let frames: u64 = opt(args, "--frames").unwrap_or("600").parse()?;
    let every: u64 = opt(args, "--every").unwrap_or("60").parse()?;
    let shots = opt(args, "--shots").map(PathBuf::from);
    let fast = flag(args, "--fast");
    let wav = opt(args, "--wav").map(PathBuf::from);
    let mut all_audio: Vec<i16> = Vec::new();
    let tty = flag(args, "--tty");
    // input script: frame:button:duration,...
    let mut script: Vec<(u64, Button, u64)> = Vec::new();
    if let Some(s) = opt(args, "--input") {
        for item in s.split(',') {
            let p: Vec<&str> = item.split(':').collect();
            if p.len() >= 2 {
                let f: u64 = p[0].parse()?;
                let b = parse_button(p[1]).context("bad button")?;
                let d: u64 = p.get(2).map(|x| x.parse()).transpose()?.unwrap_or(6);
                script.push((f, b, d));
            }
        }
    }

    let bios = std::fs::read(bios_path)?;
    let mut disc = Disc::open(&cue)?;
    let ident = dwrc_disc::identify(&mut disc)?.context("disc not recognised")?;
    println!("{} ({}) region {:?}", ident.title, ident.serial, ident.region);
    let (std, region) = match ident.region {
        dwrc_disc::ident::Region::Pal => (VideoStandard::Pal, DiscRegion::Pal),
        dwrc_disc::ident::Region::NtscJ => (VideoStandard::Ntsc, DiscRegion::NtscJ),
        dwrc_disc::ident::Region::NtscU => (VideoStandard::Ntsc, DiscRegion::NtscU),
    };
    let fs = IsoFs::mount(&mut disc)?;
    let exe_entry = fs.find(&mut disc, &ident.boot_file)?.context("boot exe missing")?;
    let exe = PsxExe::parse(&fs.read(&mut disc, &exe_entry)?)?;

    let mut psx = Psx::new(&bios, std);
    psx.bus.tty_echo = tty;
    psx.insert_disc(Box::new(disc) as Box<dyn SectorSource>, region);
    if fast {
        psx.enable_fast_boot(exe);
    }
    if let Some(d) = &shots {
        std::fs::create_dir_all(d)?;
    }
    let t0 = Instant::now();
    let mut buf = Vec::new();
    let mut audio = Vec::new();
    let mut audio_total = 0usize;
    for f in 1..=frames {
        let mut state = dwrc_psx::PadState::default();
        for (sf, b, d) in &script {
            if f >= *sf && f < sf + d {
                state.set(*b, true);
            }
        }
        psx.bus.sio.pads[0].state = state;
        psx.run_frame();
        psx.bus.spu.take_output(&mut audio);
        audio_total += audio.len();
        if wav.is_some() {
            all_audio.extend_from_slice(&audio);
        }
        if let Some(dir) = &shots {
            if f % every == 0 {
                let (w, h) = psx.bus.gpu.render_display(&mut buf);
                save_png(&dir.join(format!("frame_{f:06}.png")), w, h, &buf)?;
            }
        }
        audio.clear();
    }
    let el = t0.elapsed().as_secs_f64();
    println!(
        "{} frames in {:.2}s ({:.1} fps), pc={:08x}, {} instructions, audio samples {}",
        frames,
        el,
        frames as f64 / el,
        psx.cpu.pc,
        psx.cpu.instructions,
        audio_total / 2
    );
    if let Some(w) = &wav {
        save_wav(w, &all_audio)?;
        let peak = all_audio.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let nonzero = all_audio.iter().filter(|s| **s != 0).count();
        println!("audio: peak {peak}, non-zero samples {nonzero}/{}", all_audio.len());
    }
    if let Some(dir) = &shots {
        save_vram(&dir.join("vram.png"), &psx)?;
    }
    if !psx.bus.tty.is_empty() {
        println!("--- TTY ---\n{}", psx.bus.tty);
    }
    Ok(())
}

fn save_png(path: &Path, w: u32, h: u32, argb: &[u32]) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for p in argb {
        data.push((p >> 16) as u8);
        data.push((p >> 8) as u8);
        data.push(*p as u8);
    }
    wr.write_image_data(&data)?;
    Ok(())
}

fn save_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&44100u32.to_le_bytes());
    out.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn save_vram(path: &Path, psx: &Psx) -> Result<()> {
    let v: Vec<u32> = psx.bus.gpu.vram.iter().map(|&c| dwrc_psx::gpu::rgb15_to_argb(c)).collect();
    save_png(path, 1024, 512, &v)
}

fn disasm(args: &[String]) -> Result<()> {
    let data = std::fs::read(&args[0])?;
    let exe = PsxExe::parse(&data)?;
    let addr = u32::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
    let count: u32 = args[2].parse()?;
    for i in 0..count {
        let a = addr + i * 4;
        let off = (a - exe.load_addr) as usize;
        if off + 4 > exe.text.len() {
            break;
        }
        let w = u32::from_le_bytes(exe.text[off..off + 4].try_into().unwrap());
        println!("{a:08x}: {w:08x}  {}", dwrc_mips::disasm(a, dwrc_mips::Instr(w)));
    }
    Ok(())
}

fn inspect(path: PathBuf) -> Result<()> {
    let mut disc = Disc::open(&path)?;
    for t in disc.tracks() {
        println!("track {:02} {:?} start {} len {}", t.number, t.kind, t.start_msf(), t.length);
    }
    let fs = IsoFs::mount(&mut disc)?;
    println!("volume '{}' system '{}' sectors {}", fs.volume_id, fs.system_id, fs.volume_sectors);
    if let Some(id) = dwrc_disc::identify(&mut disc)? {
        println!("identified: {:?}", id);
    } else {
        println!("not identified");
    }
    for (p, e) in fs.walk(&mut disc)? {
        println!("{:>8} {:>10} {}{}", e.lba, e.size, p, if e.is_dir { "/" } else { "" });
    }
    if let Some(cnf) = fs.find(&mut disc, "SYSTEM.CNF")? {
        let text = String::from_utf8_lossy(&fs.read(&mut disc, &cnf)?).to_string();
        println!("--- SYSTEM.CNF ---\n{text}");
        let kv = dwrc_disc::iso9660::parse_system_cnf(&text);
        if let Some((_, b)) = kv.iter().find(|(k, _)| k == "BOOT") {
            let bp = dwrc_disc::iso9660::boot_path(b);
            if let Some(e) = fs.find(&mut disc, &bp)? {
                let exe = PsxExe::parse(&fs.read(&mut disc, &e)?)?;
                println!(
                    "exe {}: pc={:08x} gp={:08x} load={:08x} size={:#x} bss={:08x}+{:#x} sp={:08x} marker='{}'",
                    bp, exe.pc, exe.gp, exe.load_addr, exe.text_size, exe.bss_addr, exe.bss_size, exe.sp_base, exe.region_marker
                );
            }
        }
    }
    Ok(())
}
