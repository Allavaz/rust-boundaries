use clap::Parser;
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{prelude::*, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to the playlist
    #[arg()]
    path: PathBuf,

    /// LU below average loudness to trigger next track
    #[arg(short, long, default_value_t = 8.)]
    level: f32,

    /// LU below average loudness for track cue-in point
    #[arg(short, long, default_value_t = 40.)]
    cue: f32,
}

struct AnalyzeResult {
    start_next: f32,
    cue_point: f32,
    duration: f32,
    loudness: f32,
    path: String,
}

fn first_time_threshold(measure: &Vec<(f32, f32)>, threshold: f32, rev: bool) -> f32 {
    let iter: Box<dyn Iterator<Item = &(f32, f32)>> = if rev {
        Box::new(measure.iter().rev())
    } else {
        Box::new(measure.iter())
    };
    for item in iter {
        if item.1 > threshold {
            return item.0;
        }
    }
    0.
}

fn analyze(path: &str, vol_drop: f32, vol_start: f32) -> AnalyzeResult {
    println!("Processing filename: {}", path);

    let test = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-vn")
        .arg("-af")
        .arg("ebur128")
        .arg("-f")
        .arg("null")
        .arg("null")
        .output()
        .unwrap();

    let test = match String::from_utf8(test.stderr) {
        Ok(t) => t,
        Err(_) => panic!("Failed on filename: {path}"),
    };

    let test: Vec<&str> = test.lines().collect();

    let mut measure: Vec<(f32, f32)> = Vec::new();

    for i in 0..test.len() {
        if i > (test.len() - 13) || !test[i].starts_with("[Parsed_ebur128") {
            continue;
        }
        let t_i = match test[i].find("t:") {
            None => continue,
            Some(i) => i,
        };
        let t: f32 = test[i][t_i + 2..t_i + 8].trim().parse().unwrap();
        let m_i = match test[i].find("M:") {
            None => continue,
            Some(i) => i,
        };
        let m: f32 = test[i][m_i + 2..m_i + 8].trim().parse().unwrap();
        measure.push((t, m))
    }

    // measure now contains a list of lists-of-floats: each item is [time],[loudness]

    let loudness: f32 = test[test.len() - 8][15..20].trim().parse().unwrap();

    let partially_parsed_duration = &test[test.len() - 13][14..25];
    let hms_split: Vec<&str> = partially_parsed_duration.split(":").collect();
    let hours = hms_split[0].parse::<f32>().unwrap() * 3600.00;
    let minutes = hms_split[1].parse::<f32>().unwrap() * 60.00;
    let seconds = hms_split[2].parse::<f32>().unwrap();
    let duration = hours + minutes + seconds;

    let cue_level = loudness - vol_start;

    let ebu_cue_time = first_time_threshold(&measure, cue_level, false);

    let cue_time = f32::max(0., ebu_cue_time - 0.4);

    let mut next_level = loudness - vol_drop;
    let mut next_time = first_time_threshold(&measure, next_level, true);

    if duration - next_time > 15. {
        next_level = loudness - vol_drop - 15.;
        next_time = first_time_threshold(&measure, next_level, true);
    }

    let start_next = f32::max(duration - next_time, 0.);

    AnalyzeResult {
        start_next,
        cue_point: cue_time,
        duration,
        loudness,
        path: path.to_string(),
    }
}

fn main() {
    let args = Args::parse();

    let mut playlist_lines: Vec<String> = Vec::new();

    let playlist_path = args.path.to_path_buf();

    let mut new_path = PathBuf::from(playlist_path);
    new_path.pop();
    new_path.push("new-playlist.m3u");

    let file = File::open(&args.path).unwrap();
    let reader = BufReader::new(file);

    for line in reader.lines() {
        if let Ok(s) = line {
            if s != "#EXTM3U" {
                playlist_lines.push(s);
            }
        }
    }

    let results = Arc::new(Mutex::new(Vec::<Option<AnalyzeResult>>::new()));

    for _line in &playlist_lines {
        results.lock().unwrap().push(None);
    }

    playlist_lines
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, op)| {
            let r = analyze(&op, args.level, args.cue);
            results.lock().unwrap()[i] = Some(r);
        });

    let new_file = match OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&new_path)
    {
        Ok(fd) => fd,
        Err(_) => File::create(&new_path).unwrap(),
    };
    let mut writer = BufWriter::new(new_file);

    for result in results.lock().unwrap().iter() {
        match result {
            Some(result) => write!(writer, "annotate:liq_queue_in=\"{:.3}\", liq_cross_duration=\"{:.3}\", duration=\"{:.3}\", liq_amplify=\"{:.3}dB\":{}\n", 
            result.cue_point, result.start_next, result.duration, (-23.) - result.loudness, result.path).unwrap(),
            None => continue
        }
    }
}
