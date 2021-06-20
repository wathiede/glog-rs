use log::{Log, Record, Metadata, Level};
use std::cell::RefCell;
use termcolor::StandardStream;
use thread_local::CachedThreadLocal;
use termcolor::{ColorSpec, ColorChoice, Color, WriteColor};
use std::io::{LineWriter, Write};
use std::path::Path;
use chrono::Local;
use chrono::DateTime;
use std::convert::TryInto;
use backtrace::Backtrace;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::str::FromStr;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::sync::{Arc, Mutex};
use if_empty::*;

mod flags;

pub use flags::Flags as Flags;

pub struct Glog {
    stderr_writer: CachedThreadLocal<RefCell<StandardStream>>,
    compatible_verbosity: bool,
    compatible_date: bool,
    flags: Flags,
    application_fingerprint: Option<String>,
    start_time: DateTime<Local>,
    file_writer: HashMap<Level, Arc<Mutex<RefCell<File>>>>,
}

impl Glog {
    pub fn new() -> Glog {
        Glog {
            stderr_writer: CachedThreadLocal::new(),
            compatible_verbosity: true,
            compatible_date: true,
            flags: Flags::default(),
            application_fingerprint: None,
            start_time: Local::now(),
            file_writer: HashMap::new(),
        }
    }
    pub fn init(&mut self, flags: Flags) -> Result<(), log::SetLoggerError> {
        self.flags = flags;
        if !self.flags.logtostderr {
            self.create_log_files();
        }
        // todo: restore this once this can be changed during runtime for glog
        // log::set_max_level(LevelFilter::Trace);
        log::set_max_level(self.flags.minloglevel.to_level_filter());
        log::set_boxed_logger(Box::new(self.clone()))
    }

    pub fn with_year(mut self, with_year: bool) -> Self {
        self.compatible_date = !with_year;
        self
    }

    pub fn limited_abbreviations(mut self, limit_abbreviations: bool) -> Self {
        self.compatible_verbosity = limit_abbreviations;
        self
    }

    pub fn set_application_fingerprint(mut self, fingerprint: &str) -> Self {
        self.application_fingerprint = Some(fingerprint.to_owned());
        self
    }

    fn match_level(&self, level: &Level) -> Level {
        match level {
            Level::Debug if self.compatible_verbosity => Level::Info,
            Level::Trace if self.compatible_verbosity => Level::Info,
            _ => *level,
        }
    }

    fn create_log_files(&mut self) {
        let log_file_dir = self.flags.log_dir.clone();
        let mut log_file_name = OsString::new();
        log_file_name.push(std::env::current_exe().unwrap_or(PathBuf::from_str("UNKNOWN").unwrap_or(PathBuf::new())).file_name().unwrap_or(OsStr::new("UNKNOWN")));
        log_file_name.push(".");
        log_file_name.push(gethostname::gethostname().if_empty(OsString::from("(unknown)")));
        log_file_name.push(".");
        log_file_name.push(whoami::username().if_empty(String::from("invalid-user")));
        log_file_name.push(".log.");

        // todo: plain String may suffice here
        let mut log_file_suffix = OsString::new();
        log_file_suffix.push(".");
        log_file_suffix.push(Local::now().format("%Y%m%d-%H%M%S").to_string());
        log_file_suffix.push(".");
        log_file_suffix.push(std::process::id().to_string());

        let mut log_file_base = OsString::new();
        log_file_base.push(log_file_dir);
        log_file_base.push(log_file_name);
        if !self.compatible_verbosity {
            for level in vec![Level::Trace, Level::Debug] {
                let mut log_file_path = log_file_base.clone();
                log_file_path.push(level.to_string().to_uppercase());
                log_file_path.push(log_file_suffix.clone());
                self.write_file_header(&log_file_path, &level);
            }
        }
        for level in vec![Level::Info, Level::Warn, Level::Error] {
            let mut log_file_path = log_file_base.clone();
            log_file_path.push(level.to_string().to_uppercase());
            log_file_path.push(log_file_suffix.clone());
            self.write_file_header(&log_file_path, &level);
        }
    }

    fn write_file_header(&mut self, file_path: &OsString, level: &Level) {
        {
            let mut file = match File::create(&file_path) {
                Err(why) => panic!("couldn't create {}: {}", file_path.to_str().unwrap_or("<INVALID FILE PATH>"), why),
                Ok(file) => file,
            };

            let running_duration = Local::now() - self.start_time;

            // todo: integrate UTC
            file.write_fmt(
                format_args!("Log file created at:\n{}\nRunning on machine: {}\n{}Running duration (h:mm:ss): {}:{:02}:{:02}\nLog line format: [{}IWE]{}mmdd hh:mm:ss.uuuuuu threadid file:line] msg\n",
                    Local::now().format("%Y/%m/%d %H:%M:%S"),
                    gethostname::gethostname().to_str().unwrap_or("UNKNOWN"),
                    if self.application_fingerprint.is_some() { format!("Application fingerprint: {}\n", self.application_fingerprint.clone().unwrap()) } else { String::new() },
                    running_duration.num_hours(),
                    running_duration.num_minutes(),
                    running_duration.num_seconds(),
                    if self.compatible_verbosity { "" } else { "TD" },
                    if self.compatible_date { "" } else { "yyyy" },
                )
            ).expect("couldn't write log file header");

            match file.flush() {
                Err(why) => panic!("couldn't flush {} after writing file header: {}", file_path.to_str().unwrap(), why),
                _ => (),
            }
        }
        self.file_writer.insert(*level, Arc::new(Mutex::new(RefCell::new(OpenOptions::new().append(true).open(&file_path).expect("Couldn't open file after header is written")))));
    }

    fn should_log_backtrace(&self, file_name: &str, line: u32) -> bool {
        if self.flags.log_backtrace_at.is_some() {
            // todo: improve this by formatting this beforehand
            format!("{}:{}", file_name, line) == *self.flags.log_backtrace_at.as_ref().unwrap()
        } else {
            false
        }
    }

    fn record_to_file_name(record: &Record) -> String {
        Path::new(record.file().unwrap_or("")).file_name().unwrap_or(std::ffi::OsStr::new("")).to_os_string().into_string().unwrap_or("".to_owned())
    }

    fn build_log_message(&self, record: &Record) -> String {
        format!("{}{} {:5} {}:{}] {}",
            self.match_level(&record.metadata().level()).as_str().chars().nth(0).unwrap(),
            Local::now().format(
                &format!("{}%m%d %H:%M:%S%.6f",
                    if self.compatible_date { "" } else { "%Y" }
                )
            ),
            get_tid(),
            Glog::record_to_file_name(record),
            record.line().unwrap(),
            record.args(),
        )
    }

    fn write_stderr(&self, record: &Record) {
        let stderr_writer = self.stderr_writer.get_or(|| RefCell::new(StandardStream::stderr(ColorChoice::Auto)));
        let stderr_writer = stderr_writer.borrow_mut();
        let mut stderr_writer = LineWriter::new(stderr_writer.lock());

        if self.flags.colorlogtostderr {
            stderr_writer.get_mut().set_color(ColorSpec::new().set_fg(match record.metadata().level() {
                Level::Error => Some(Color::Red),
                Level::Warn => Some(Color::Yellow),
                _ => None
            })).expect("failed to set color");
        }

        let file_name = Glog::record_to_file_name(record);

        writeln!(stderr_writer, "{}", self.build_log_message(record)).expect("couldn't write log message");

        if self.flags.colorlogtostderr {
            stderr_writer.get_mut().reset().expect("failed to reset color");
        }

        if self.should_log_backtrace(&file_name, record.line().unwrap_or(0)) {
            writeln!(stderr_writer, "{:?}", Backtrace::new()).expect("Couldn't write backtrace");
        }
    }

    fn write_file(&self, record: &Record) {
        let level = self.match_level(&record.level());
        let file_write_guard = self.file_writer.get(&level).unwrap().lock().unwrap();
        let mut file_writer = (*file_write_guard).borrow_mut();
        match file_writer.write_fmt(format_args!("{}\n", self.build_log_message(record))) {
            Err(why) => panic!("couldn't write log message to file for level {}: {}", record.level(), why),
            _ => (),
        };
    }

    fn write_sinks(&self) {
    
    }
}

impl Log for Glog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.flags.minloglevel >= metadata.level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return
        }

        if self.flags.logtostderr || self.flags.alsologtostderr {
            self.write_stderr(record);
        }
        if !self.flags.logtostderr {
            self.write_file(record);
        }
        self.write_sinks();
    }

    fn flush(&self) {
        let stderr_writer = self.stderr_writer.get_or(|| RefCell::new(StandardStream::stderr(ColorChoice::Auto)));
        let mut stderr_writer = stderr_writer.borrow_mut();
        stderr_writer.flush().ok();

        for file in self.file_writer.values() {
            let file_guard = file.lock().unwrap();
            let mut file_writer = (*file_guard).borrow_mut();
            file_writer.flush().expect("couldn't sync log to disk");
        }
    }
}

#[cfg(target_os = "macos")]
fn get_tid() -> u64 {
    nix::sys::pthread::pthread_self().try_into().unwrap()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn get_tid() -> u64 {
    nix::unistd::gettid().as_raw().try_into().unwrap()
}

#[cfg(target_os = "windows")]
fn get_tid() -> u64 {
    bindings::Windows::Win32::System::Threading::GetCurrentThreadId().try_into().unwrap()
}

impl Clone for Glog {
    fn clone(&self) -> Glog {
        Glog {
            stderr_writer: CachedThreadLocal::new(),
            flags: self.flags.clone(),
            application_fingerprint: self.application_fingerprint.clone(),
            file_writer: self.file_writer.clone(),
            ..*self
        }
    }
}

impl Default for Glog {
    fn default() -> Self {
        Glog::new()
    }
}

pub fn new() -> Glog {
    Glog::new()
}
