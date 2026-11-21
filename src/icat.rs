//! `niruterm icat FILE...`: show pictures in the terminal with the kitty
//! graphics protocol (also works in kitty). PNG is sent as it is; other
//! formats (JPEG, WebP, GIF, SVG...) and videos go through the system's
//! thumbnailers first, the same way NIRUFM makes its previews.
use std::io::Write;

fn term_size() -> (u16, u16, u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
    (ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel)
}

fn thumbnailer_for(mime_hint: &str) -> Option<String> {
    let dirs = ["/usr/share/thumbnailers", "/usr/local/share/thumbnailers"];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let exec = t.lines().find_map(|l| l.strip_prefix("Exec="));
            let types = t
                .lines()
                .find_map(|l| l.strip_prefix("MimeType="))
                .unwrap_or("");
            if types.split(';').any(|m| m == mime_hint) {
                return exec.map(str::to_string);
            }
        }
    }
    None
}

fn guess_mime(path: &str) -> &'static str {
    let l = path.to_lowercase();
    let ext = l.rsplit('.').next().unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// PNG bytes of a picture, at most `max` pixels on a side.
fn png_of(path: &str, max: u32) -> Result<Vec<u8>, String> {
    let mime = guess_mime(path);
    if mime == "image/png" {
        return std::fs::read(path).map_err(|e| e.to_string());
    }
    let tmp = std::env::temp_dir().join(format!("niruterm-icat-{}.png", std::process::id()));
    let ok = if mime == "application/pdf" {
        let prefix = tmp.with_extension("");
        std::process::Command::new("pdftoppm")
            .args([
                "-png",
                "-f",
                "1",
                "-l",
                "1",
                "-singlefile",
                "-scale-to",
                &max.to_string(),
            ])
            .arg(path)
            .arg(&prefix)
            .status()
            .is_ok_and(|s| s.success())
    } else {
        let exec = thumbnailer_for(mime)
            .ok_or_else(|| format!("{path}: no program on this system can read {mime}"))?;
        let abs = std::fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
        let uri = format!("file://{}", abs.display());
        let words: Vec<String> = exec
            .split_whitespace()
            .map(|w| {
                w.replace("%s", &max.to_string())
                    .replace("%u", &uri)
                    .replace("%i", &abs.to_string_lossy())
                    .replace("%o", &tmp.to_string_lossy())
            })
            .collect();
        let Some((cmd, rest)) = words.split_first() else {
            return Err("bad thumbnailer".into());
        };
        std::process::Command::new(cmd)
            .args(rest)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let data = std::fs::read(&tmp).map_err(|_| format!("{path}: could not convert"));
    let _ = std::fs::remove_file(&tmp);
    if !ok {
        return Err(format!("{path}: could not convert"));
    }
    data
}

fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || &data[1..4] != b"PNG" {
        return None;
    }
    Some((
        u32::from_be_bytes(data[16..20].try_into().ok()?),
        u32::from_be_bytes(data[20..24].try_into().ok()?),
    ))
}

pub fn run(args: Vec<String>) -> i32 {
    let mut width: Option<u32> = None;
    let mut files = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--width" | "-w" => width = it.next().and_then(|v| v.parse().ok()),
            "--help" | "-h" => {
                println!("niruterm icat [--width COLS] FILE...   show pictures (PNG, JPEG, WebP, GIF, SVG, PDF page, video frame)");
                return 0;
            }
            _ => files.push(a),
        }
    }
    if files.is_empty() {
        eprintln!("niruterm icat: name a picture");
        return 2;
    }
    let (cols, _rows, xpx, _ypx) = term_size();
    let cell_w = if cols > 0 && xpx > 0 {
        xpx as u32 / cols as u32
    } else {
        9
    };
    let max_px = if xpx > 0 {
        (xpx as u32).clamp(200, 2048)
    } else {
        1200
    };
    let mut out = std::io::stdout().lock();
    let mut status = 0;
    for f in files {
        let data = match png_of(&f, max_px) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("niruterm icat: {e}");
                status = 1;
                continue;
            }
        };
        // Wider than the terminal: fit to its width.
        let fit_cols = match (width, png_size(&data)) {
            (Some(w), _) => Some(w),
            (None, Some((w, _))) if cols > 0 && w > cell_w * cols as u32 => Some(cols as u32),
            _ => None,
        };
        let enc = crate::graphics::base64_encode(&data);
        let chunks: Vec<&str> = enc
            .as_bytes()
            .chunks(4096)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        for (k, chunk) in chunks.iter().enumerate() {
            let more = u8::from(k + 1 < chunks.len());
            if k == 0 {
                let size = fit_cols.map(|c| format!(",c={c}")).unwrap_or_default();
                let _ = write!(out, "\x1b_Ga=T,f=100,q=2{size},m={more};{chunk}\x1b\\");
            } else {
                let _ = write!(out, "\x1b_Gm={more};{chunk}\x1b\\");
            }
        }
        let _ = writeln!(out);
    }
    let _ = out.flush();
    status
}
