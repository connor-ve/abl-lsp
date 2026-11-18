//! `abl logs`: the database log files (`<db>.lg`, next to the `.db`) of the
//! databases abl manages: the last lines, filtered, or followed as they grow.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use abl_oe::config;

fn read_lossy(p: &Path) -> String {
    fs::read(p)
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default()
}

fn matches(line: &str, grep: Option<&str>) -> bool {
    grep.is_none_or(|g| line.to_lowercase().contains(&g.to_lowercase()))
}

pub fn run(
    cwd: &Path,
    name: Option<&str>,
    lines: usize,
    grep: Option<&str>,
    follow: bool,
) -> Result<u8, String> {
    let l = config::load_project(cwd)?.ok_or("no abl.toml here or above; run `abl init`")?;
    let mut logs: Vec<(String, PathBuf)> = l
        .project
        .db
        .iter()
        .filter(|(n, d)| d.connect.is_none() && name.is_none_or(|w| w.eq_ignore_ascii_case(n)))
        .map(|(n, d)| (n.clone(), l.db_path(n, d).with_extension("lg")))
        .collect();
    if logs.is_empty() {
        return Err(match name {
            Some(n) => format!("no database {n} managed by abl in abl.toml"),
            None => "no databases managed by abl in abl.toml".into(),
        });
    }
    logs.sort();
    let several = logs.len() > 1;
    for (i, (n, p)) in logs.iter().enumerate() {
        if several {
            if i > 0 {
                println!();
            }
            println!("== {n} ==");
        }
        if !p.is_file() {
            println!("(no log yet: {} is created with the database)", p.display());
            continue;
        }
        let text = read_lossy(p);
        let picked: Vec<&str> = text.lines().filter(|x| matches(x, grep)).collect();
        for line in &picked[picked.len().saturating_sub(lines)..] {
            println!("{line}");
        }
    }
    if !follow {
        return Ok(0);
    }
    // Follow: print what is appended, polling twice a second.
    let mut pos: Vec<u64> = logs
        .iter()
        .map(|(_, p)| fs::metadata(p).map_or(0, |m| m.len()))
        .collect();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        for (i, (n, p)) in logs.iter().enumerate() {
            let len = fs::metadata(p).map_or(0, |m| m.len());
            if len < pos[i] {
                pos[i] = 0; // truncated or recreated
            }
            if len == pos[i] {
                continue;
            }
            let Ok(mut f) = fs::File::open(p) else {
                continue;
            };
            let mut buf = Vec::new();
            if f.seek(SeekFrom::Start(pos[i])).is_ok() && f.read_to_end(&mut buf).is_ok() {
                // Only whole lines; the rest waits for the next poll.
                let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |e| e + 1);
                for line in String::from_utf8_lossy(&buf[..end]).lines() {
                    if matches(line, grep) {
                        if several {
                            println!("{n}: {line}");
                        } else {
                            println!("{line}");
                        }
                    }
                }
                pos[i] += end as u64;
            }
        }
    }
}
