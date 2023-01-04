use clap::Parser;
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{prelude::*, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
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

    /// Output filename (default: '-processed' suffix)
    #[arg(short, long, default_value_t = String::from(""))]
    output: String,

    /// Append to output file instead of overwriting everything
    #[arg(short, long, default_value_t = false)]
    append: bool,
}

#[derive(Default)]
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
    /*
    Analyses file in filename, returns seconds to end-of-file of place where volume last drops to level
    below average loudness, given in volDrop in LU.
    Also determines file start, where monentary loudness leaps above a certain point given by volStart
    Also encode and store a mezzanine file, if a mezzanine directory name is given
    Make a list containing many points, 1/10 sec apart, where loudness is measured.
    We need TIME and MOMENTARY LOUDNESS
    We also need full INTEGRATED LOUDNESS
    */

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
    // We pass "-vn" because some music files have invalid images, which can't be processed by ffmpeg

    // from_utf8_lossy replaces wrong chars with question marks preventing crashes
    let test = String::from_utf8_lossy(&test.stderr).to_string();

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
    // measure now contains a vector of a 2-float tuples: each item is ([time], [loudness])

    if measure.len() == 0 {
        panic!("Couldn't measure filename: {}", path);
    }

    // get integrated loudness
    let loudness: f32 = test[test.len() - 8][15..20].trim().parse().unwrap();

    // parse duration from the status line
    let partially_parsed_duration = &test[test.len() - 13][14..25];
    let hms_split: Vec<&str> = partially_parsed_duration.split(":").collect();
    let hours = hms_split[0].parse::<f32>().unwrap() * 3600.00;
    let minutes = hms_split[1].parse::<f32>().unwrap() * 60.00;
    let seconds = hms_split[2].parse::<f32>().unwrap();
    let duration = hours + minutes + seconds;

    /*
    First, let us find the first timestamp where the momentary loudness is volStart below the
    track's overall loudness level. That level is cueLevel
    */
    let cue_level = loudness - vol_start;

    let ebu_cue_time = first_time_threshold(&measure, cue_level, false);

    /*
    The EBU R.128 algorithm measures in 400ms blocks. Therefore, it marks 0.4s as the
    start of the track, even if its audio begins at 0.0s. So, we must subtract 400ms
    from the given time, then use either that time, or 0.0s (if the result is negative)
    as our track starting point.
    */
    let cue_time = f32::max(0., ebu_cue_time - 0.4);

    /*
    Now we must find the last timestamp where the momentary loudness is volDrop LU
    below the track's overall loudness level. That level is nextLevel.
    */
    let mut next_level = loudness - vol_drop;
    let mut next_time = first_time_threshold(&measure, next_level, true);

    /*
    Little piece of logic to fix "Bohemian Rhapsody" and other songs with a long
    but important tail.
    */
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

    let use_custom_path = if args.output.ne("") { true } else { false };

    let custom_pathbuf = PathBuf::from(args.output);

    println!("Processing playlist: {}", args.path.display());

    // remove last piece from the path of the original playlist and add the new one
    let mut new_path = PathBuf::from(playlist_path);
    let file_stem = match new_path.file_stem() {
        Some(s) => s.to_string_lossy().to_string(),
        None => panic!("Wrong output path"),
    };
    let new_filename = format!("{}-processed.m3u8", file_stem);
    new_path.set_file_name(new_filename);

    let file = File::open(&args.path).unwrap();
    let reader = BufReader::new(file);

    for line in reader.lines() {
        if let Ok(s) = line {
            if s != "#EXTM3U\n" {
                playlist_lines.push(s);
            }
        }
    }

    /*
    We could just push the AnalyzeResults to the vector as they come, but since
    we're doing this with threads, that would mess up the order of the tracks,
    which may not be desired. So we make a vector with n "empty slots"
    (for n tracks in the playlist) pushing n AnalyzeResults with default values
    to the vector.
    */
    let results = Arc::new(Mutex::new(Vec::<AnalyzeResult>::new()));

    for _line in &playlist_lines {
        results.lock().unwrap().push(Default::default());
    }

    playlist_lines
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, op)| {
            let r = analyze(&op, args.level, args.cue);
            results.lock().unwrap()[i] = r;
        });

    println!(
        "Done with analysis, now {} to output playlist: {}",
        if args.append { "appending" } else { "writing" },
        if use_custom_path {
            custom_pathbuf.display()
        } else {
            new_path.display()
        }
    );

    let mut write_options = OpenOptions::new();
    write_options.write(true);
    if args.append {
        write_options.append(true)
    } else {
        write_options.truncate(true)
    };

    let new_file = match write_options.open(if use_custom_path {
        &custom_pathbuf
    } else {
        &new_path
    }) {
        Ok(fd) => fd,
        Err(_) => File::create(if use_custom_path {
            &custom_pathbuf
        } else {
            &new_path
        })
        .unwrap(),
    };
    let mut writer = BufWriter::new(new_file);

    /*
    We save the whole thing into a big string and then write that to avoid
    writing (and saving) to the file multiple times unncecesarily
    */
    let mut result_string = String::new();

    if !args.append {
        result_string.push_str("#EXTM3U\n");
    }

    for result in results.lock().unwrap().iter() {
        let annotate = format!("annotate:liq_cue_in=\"{:.3}\",liq_cross_duration=\"{:.3}\",duration=\"{:.3}\",liq_amplify=\"{:.3}dB\":{}\n", 
        result.cue_point, result.start_next, result.duration, (-23.) - result.loudness, result.path);
        result_string.push_str(&annotate);
    }

    write!(writer, "{result_string}").unwrap();

    println!("Done!")
}
