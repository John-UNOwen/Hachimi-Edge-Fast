use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};
use std::fs::File;

pub fn init(filter_level: log::LevelFilter, file_logging: bool) {
    // Why file logging fell back, if it was asked for and did not start. Reported once the debug
    // output logger is up, so the reason reaches the debugger instead of being dropped.
    let mut file_failure: Option<String> = None;

    if file_logging {
        let mut path = super::utils::get_game_dir();
        path.push("hachimi.log");

        match File::create(&path) {
            Ok(file) => {
                let config = ConfigBuilder::new()
                    .set_target_level(LevelFilter::Error)
                    .add_filter_ignore_str("sqlparser")
                    .set_time_format_rfc3339()
                    .build();

                match WriteLogger::init(filter_level, config, file) {
                    Ok(()) => return,
                    Err(e) => file_failure = Some(format!("the logger could not be installed: {e}")),
                }
            }
            Err(e) => file_failure = Some(format!("{}: {e}", path.display())),
        }
    }

    if let Some(level) = filter_level.to_level() {
        windebug_logger::init_with_level(level).ok();
    }

    if let Some(reason) = file_failure {
        error!("File logging was requested but did not start, logging to the debug output only: {reason}");
    }
}