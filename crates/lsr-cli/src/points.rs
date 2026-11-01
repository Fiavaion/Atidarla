//! `lasair points convert|info` (docs/point-caches.md): the point-cache
//! converter. Reads PLY, CSV, XYZ/PTS and `.lsrpc`, writes `.lsrpc`, PLY or
//! CSV; a `$F` in both names converts a numbered sequence.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use lsr_ops_pnt::cache::{self, Format, frame_path};

#[derive(Debug, Subcommand)]
pub enum PointsCommand {
    /// Convert a point file (or a sequence: `$F` or `$F4` in both names,
    /// with --range) to `.lsrpc`, PLY or CSV, chosen by OUT's extension.
    /// Houdini `.bgeo` and other closed caches: export PLY or CSV first
    /// (Houdini's `gconvert in.bgeo out.ply`), then convert that.
    Convert {
        /// The file to read: .lsrpc, .ply, .csv, .xyz, .pts or .txt.
        input: PathBuf,
        /// The file to write: .lsrpc (default for Point File In), .ply or .csv.
        output: PathBuf,
        /// Frames START-END of a sequence, END inclusive (for example 1-240).
        #[arg(long, value_parser = parse_frames)]
        range: Option<(i64, i64)>,
        /// Multiplies every position (unit conversion: 0.01 for centimetres
        /// to metres).
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
    },
    /// Print a point file's count and attributes.
    Info { file: PathBuf },
}

fn parse_frames(s: &str) -> Result<(i64, i64), String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("`{s}`: use START-END, for example 1-240"))?;
    let a: i64 = a
        .trim()
        .parse()
        .map_err(|_| format!("`{a}` is not a frame"))?;
    let b: i64 = b
        .trim()
        .parse()
        .map_err(|_| format!("`{b}` is not a frame"))?;
    if b < a {
        return Err(format!("`{s}`: the end comes before the start"));
    }
    Ok((a, b))
}

pub fn run(cmd: PointsCommand) -> Result<()> {
    match cmd {
        PointsCommand::Convert {
            input,
            output,
            range,
            scale,
        } => {
            let n = convert(&input, &output, range, scale)?;
            println!("converted {n} file(s)");
            Ok(())
        }
        PointsCommand::Info { file } => {
            let c = cache::read(&file).map_err(anyhow::Error::msg)?;
            println!("{}: {} points", file.display(), c.count);
            for col in &c.columns {
                println!("  {} ({})", col.name, col.ty);
            }
            Ok(())
        }
    }
}

/// Converts one file, or every frame of `range` when the names hold `$F`;
/// returns the number of files written.
pub fn convert(
    input: &Path,
    output: &Path,
    range: Option<(i64, i64)>,
    scale: f32,
) -> Result<usize> {
    let format = Format::from_path(output)
        .filter(|f| *f != Format::Xyz)
        .with_context(|| format!("{}: write .lsrpc, .ply or .csv", output.display()))?;
    let (i, o) = (input.to_string_lossy(), output.to_string_lossy());
    let sequence = i.contains("$F");
    if sequence != o.contains("$F") {
        bail!("put `$F` (or `$F4`) in both names to convert a sequence, or in neither");
    }
    let frames = match (sequence, range) {
        (true, Some((a, b))) => a..=b,
        (true, None) => bail!("a sequence needs --range START-END"),
        (false, Some(_)) => bail!("--range needs `$F` in the file names"),
        (false, None) => 0..=0,
    };
    let mut written = 0;
    for f in frames {
        let src = PathBuf::from(frame_path(&i, f));
        let dst = PathBuf::from(frame_path(&o, f));
        let mut c = cache::read(&src).map_err(anyhow::Error::msg)?;
        if scale != 1.0 {
            for col in c.columns.iter_mut().filter(|c| c.name == "P") {
                for w in &mut col.words {
                    *w = (f32::from_bits(*w) * scale).to_bits();
                }
            }
        }
        let bytes = cache::write(&c, format).map_err(anyhow::Error::msg)?;
        if let Some(dir) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
        }
        std::fs::write(&dst, bytes).with_context(|| dst.display().to_string())?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_sequence_and_scales_positions() {
        let dir = std::env::temp_dir().join(format!("lsr-points-convert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 1..=2 {
            std::fs::write(
                dir.join(format!("in.{f}.csv")),
                format!("x,y,z,heat\n{f},0,100,0.5\n"),
            )
            .unwrap();
        }
        let n = convert(
            &dir.join("in.$F.csv"),
            &dir.join("out/c.$F4.lsrpc"),
            Some((1, 2)),
            0.01,
        )
        .unwrap();
        assert_eq!(n, 2);
        let c = cache::read(&dir.join("out/c.0002.lsrpc")).unwrap();
        assert_eq!(c.count, 1);
        let p = c.column("P").unwrap().get(0);
        assert!((p[0] - 0.02).abs() < 1e-6 && (p[2] - 1.0).abs() < 1e-6);
        assert_eq!(c.column("heat").unwrap().get(0)[0], 0.5);
        // A single file to PLY and back.
        convert(
            &dir.join("out/c.0001.lsrpc"),
            &dir.join("one.ply"),
            None,
            1.0,
        )
        .unwrap();
        assert_eq!(cache::read(&dir.join("one.ply")).unwrap().count, 1);
        assert!(convert(&dir.join("in.$F.csv"), &dir.join("x.lsrpc"), None, 1.0).is_err());
        assert!(convert(&dir.join("one.ply"), &dir.join("x.xyz"), None, 1.0).is_err());
        assert_eq!(
            parse_frames("3-1").unwrap_err(),
            "`3-1`: the end comes before the start"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
