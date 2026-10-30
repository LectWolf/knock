//! 客户端日志：写到 exe 同目录的 `latest.log`。
//!
//! 为什么不用 log/env_logger：这里要的东西很少——**一个文件、一行一条、带本地时间**，
//! 而且必须写在自己能拿到的地方（用户能直接双击打开，不用翻 AppData）。
//!
//! 规矩：
//!   - 启动时开一次文件，之后全局可用；写失败也不能影响功能（吞掉错误）
//!   - 单文件超过 `MAX_BYTES` 就滚成 `previous.log`，只留最近两份
//!   - 每条都带本地时间、级别、内容；出错的行前面加 `!`

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 超过这个大小就换一份，免得日志无限涨。
const MAX_BYTES: u64 = 4 * 1024 * 1024;

struct Sink {
    file: File,
    path: PathBuf,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// 日志文件叫什么。
pub const FILE_NAME: &str = "latest.log";

fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// 在 `dir` 里把日志开起来。返回实际路径（失败时返回候选路径，照样能给用户看）。
///
/// 目录不可写时退到临时目录，再不行就退到内存（只打印到 stderr）——
/// **日志写不出来不该让程序打不开**。
pub fn init(dir: &Path) -> PathBuf {
    let mut path = dir.join(FILE_NAME);

    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let old = dir.join("previous.log");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
    }

    let open = |p: &Path| -> Option<File> {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new().create(true).append(true).open(p).ok()
    };

    let mut file = open(&path);
    if file.is_none() {
        path = std::env::temp_dir().join(FILE_NAME);
        file = open(&path);
    }
    let Some(file) = file else {
        return path;
    };

    let _ = SINK.set(Mutex::new(Sink {
        file,
        path: path.clone(),
    }));

    // 每次启动画一条分隔线，方便一眼看出"这次运行"从哪儿开始
    write_raw(&format!(
        "\n════════ Knock {} · {} · pid {} ════════",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::process::id()
    ));
    path
}

/// 日志落在哪儿（没初始化或初始化失败时是 None）。
pub fn path() -> Option<PathBuf> {
    let sink = SINK.get()?;
    let guard = sink.lock().ok()?;
    Some(guard.path.clone())
}

fn write_raw(text: &str) {
    if let Some(sink) = SINK.get() {
        if let Ok(mut guard) = sink.lock() {
            let _ = writeln!(guard.file, "{text}");
            let _ = guard.file.flush();
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("{text}");
    }
}

/// 写一条。`level` 用 INFO / WARN / ERROR 这种固定短词。
pub fn write(level: &str, msg: &str) {
    let mark = if level == "ERROR" || level == "WARN" {
        "!"
    } else {
        " "
    };
    write_raw(&format!("{} {mark}[{level}] {msg}", timestamp()));
}

/// 打开日志所在目录（Windows 上顺手选中文件）。给界面的"看日志"用。
pub fn reveal() -> Result<(), String> {
    let Some(p) = path() else {
        return Err("日志还没开始写".into());
    };
    if !p.exists() {
        return Err(format!("日志文件不存在：{}", p.display()));
    }
    #[cfg(target_os = "windows")]
    {
        // explorer 选中文件时，退出码不可靠，所以只看有没有启动成功
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打不开资源管理器：{e}"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let dir = p.parent().unwrap_or(&p);
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打不开文件管理器：{e}"))
    }
}

/// `info!("…")` 之类的小宏，免得到处写 `logging::write("INFO", &format!(…))`。
#[macro_export]
macro_rules! klog {
    ($level:literal, $($arg:tt)*) => {
        $crate::logging::write($level, &format!($($arg)*))
    };
}
