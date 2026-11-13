//! Command-line helper: `cc-cli <file.replay>` prints the analysis as JSON;
//! `cc-cli --scan [folder]` analyzes every replay and prints one line per match.

use std::path::PathBuf;

use chest_counter::analyzer::{analyze_file, counted};
use chest_counter::store::{list_replays, replays_dir};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--scan") => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(replays_dir);
            let (mut ok, mut failed, mut total) = (0, 0, 0);
            for (path, _, _) in list_replays(&dir) {
                let start = std::time::Instant::now();
                match analyze_file(&path) {
                    Ok(m) => {
                        ok += 1;
                        if counted(&m) {
                            total += m.chests_opened;
                        }
                        println!(
                            "{:<45} {:>5}ms chests={:<3} counted={} place={:?} player={:?} playlist={:?} version={:?}",
                            m.replay_file,
                            start.elapsed().as_millis(),
                            m.chests_opened,
                            counted(&m),
                            m.placement,
                            m.player_name,
                            m.playlist,
                            m.game_version
                        );
                    }
                    Err(e) => {
                        failed += 1;
                        println!("{:<45} ERROR {e}", path.file_name().unwrap().to_string_lossy());
                    }
                }
            }
            println!("analyzed={ok} failed={failed} counted_chests={total}");
        }
        Some(file) => match analyze_file(&PathBuf::from(file)) {
            Ok(m) => println!("{}", serde_json::to_string_pretty(&m).unwrap()),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        None => eprintln!("usage: cc-cli <file.replay> | cc-cli --scan [folder]"),
    }
}
