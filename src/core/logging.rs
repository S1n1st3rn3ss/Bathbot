use failure::Error;
use flexi_logger::{
    Age, Cleanup, Criterion, DeferredNow, Duplicate, Logger, Naming, ReconfigurationHandle,
};
use log::Record;
use once_cell::sync::OnceCell;

static LOGGER_HANDLE: OnceCell<ReconfigurationHandle> = OnceCell::new();

pub fn initialize() -> Result<(), Error> {
    let log_init_status = LOGGER_HANDLE.set(
        Logger::with_env_or_str("info")
            .duplicate_to_stderr(Duplicate::Debug)
            .log_to_file()
            .directory("logs")
            .format(log_format)
            .o_timestamp(true)
            .rotate(
                Criterion::Age(Age::Day),
                Naming::Timestamps,
                Cleanup::KeepLogAndZipFiles(10, 30),
            )
            .start_with_specfile("logconfig.toml")
            .map_err(|_| format_err!("The logging configuration couldn't be found!"))?,
    );
    if log_init_status.is_err() {
        error!("The logging system was attempted to be initalized a second time!");
    }
    Ok(())
}

pub fn log_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "[{}] {} [{}:{}] {}",
        now.now().format("%Y-%m-%d %H:%M:%S"),
        record.level(),
        record.file().unwrap_or("<unnamed>"),
        record.line().unwrap_or(0),
        &record.args()
    )
}
