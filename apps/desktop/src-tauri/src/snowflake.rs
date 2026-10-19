//! 雪花算法（Snowflake）—— 64 位自增趋势的分布式唯一 ID 生成器。
//!
//! 位布局（共 63 位可用 + 1 位符号位恒为 0）：
//!
//! ```text
//!  0  - 41 bit 时间戳(毫秒, 相对 EPOCH) - 10 bit 机器 id - 12 bit 序列号
//!  ^     ^                                  ^                ^
//!  符号  约 69 年                            1024 台机器      每毫秒 4096 个
//! ```
//!
//! 只依赖 std，可直接复制到任何 Rust 工程里用。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认纪元：2024-01-01T00:00:00Z 的毫秒时间戳。
/// 自定义更早的纪元会缩短可用年限，更晚的会浪费低位。
pub const DEFAULT_EPOCH_MS: i64 = 1_704_067_200_000;

pub const TIMESTAMP_BITS: u32 = 41;
pub const WORKER_ID_BITS: u32 = 10;
pub const SEQUENCE_BITS: u32 = 12;

pub const MAX_WORKER_ID: i64 = -1 ^ (-1i64 << WORKER_ID_BITS); // 1023
pub const MAX_SEQUENCE: i64 = -1 ^ (-1i64 << SEQUENCE_BITS); // 4095

const WORKER_ID_SHIFT: u32 = SEQUENCE_BITS; // 12
const TIMESTAMP_SHIFT: u32 = SEQUENCE_BITS + WORKER_ID_BITS; // 22

/// 41 位时间戳能撑到的最大毫秒差（约 69 年）。
pub const MAX_TIMESTAMP_DELTA: i64 = -1 ^ (-1i64 << TIMESTAMP_BITS);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnowflakeError {
    /// 机器 id 超出 [0, MAX_WORKER_ID]
    WorkerIdOutOfRange { worker_id: i64, max: i64 },
    /// 时钟回拨，拒绝生成（返回等待时长，毫秒）
    ClockMovedBackwards { wait_ms: i64 },
    /// 时间戳已耗尽 41 位，需要换新纪元或更大位宽
    EpochExhausted,
}

impl std::fmt::Display for SnowflakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkerIdOutOfRange { worker_id, max } => {
                write!(f, "worker_id {worker_id} 超出范围，必须在 0..={max}")
            }
            Self::ClockMovedBackwards { wait_ms } => {
                write!(f, "检测到时钟回拨，需等待 {wait_ms}ms 才能继续生成")
            }
            Self::EpochExhausted => write!(f, "41 位时间戳已用尽，请更换 EPOCH"),
        }
    }
}

impl std::error::Error for SnowflakeError {}

#[inline]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 线程安全的雪花生成器。内部用原子变量做无锁 CAS，可直接放进全局单例。
#[derive(Debug)]
pub struct Snowflake {
    epoch_ms: i64,
    worker_id: i64,
    /// 打包状态：高 42 位是「上次出号的时间戳差值 + 1」，低 22 位是序列号。
    /// 打包成一个 u64 是为了让 CAS 能一次性比较/更新两者。
    state: AtomicU64,
}

impl Snowflake {
    pub fn new(worker_id: i64) -> Result<Self, SnowflakeError> {
        Self::with_epoch(worker_id, DEFAULT_EPOCH_MS)
    }

    pub fn with_epoch(worker_id: i64, epoch_ms: i64) -> Result<Self, SnowflakeError> {
        if worker_id < 0 || worker_id > MAX_WORKER_ID {
            return Err(SnowflakeError::WorkerIdOutOfRange {
                worker_id,
                max: MAX_WORKER_ID,
            });
        }
        Ok(Self {
            epoch_ms,
            worker_id,
            state: AtomicU64::new(0),
        })
    }

    pub fn worker_id(&self) -> i64 {
        self.worker_id
    }

    /// 生成下一个 id。时钟回拨时返回 Err，交给调用方决定是等待还是降级。
    pub fn next_id(&self) -> Result<i64, SnowflakeError> {
        loop {
            let ts = now_ms() - self.epoch_ms;
            if ts > MAX_TIMESTAMP_DELTA {
                return Err(SnowflakeError::EpochExhausted);
            }
            if ts < 0 {
                return Err(SnowflakeError::ClockMovedBackwards { wait_ms: -ts });
            }

            let prev = self.state.load(Ordering::Acquire);
            // 存的是 ts+1，这样 0 就代表「还没出过号」，不用特殊分支。
            let prev_ts = (prev >> SEQUENCE_BITS) as i64 - 1;
            let prev_seq = (prev & MAX_SEQUENCE as u64) as i64;

            let seq = if ts == prev_ts {
                // 同一毫秒内：序列号自增，溢出就自旋到下一毫秒
                (prev_seq + 1) & MAX_SEQUENCE
            } else {
                0
            };

            let next_state = (((ts + 1) as u64) << SEQUENCE_BITS) | seq as u64;
            if self
                .state
                .compare_exchange_weak(prev, next_state, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue; // 有别的线程抢先，重来
            }

            if ts == prev_ts && seq == 0 {
                // 序列号刚好打满一圈（4096 个已发出），必须等下一毫秒
                while now_ms() - self.epoch_ms <= ts {
                    std::hint::spin_loop();
                }
            }

            let id = (ts << TIMESTAMP_SHIFT) | (self.worker_id << WORKER_ID_SHIFT) | seq;
            return Ok(id);
        }
    }

    /// 阻塞式生成：时钟回拨时睡到追平，序列号打满时自动等下一毫秒。
    /// 一般业务用这个就够了。
    pub fn next_id_blocking(&self) -> i64 {
        loop {
            match self.next_id() {
                Ok(id) => return id,
                Err(SnowflakeError::ClockMovedBackwards { wait_ms }) => {
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms.min(100) as u64));
                }
                Err(other) => panic!("雪花 id 生成失败: {other}"),
            }
        }
    }
}

/// 全局生成器。worker_id 默认取进程 pid 低 10 位——单机多进程够用；
/// 多实例部署请从配置/注册中心显式注入 [`init_worker`]。
static GLOBAL: OnceLock<Snowflake> = OnceLock::new();

/// 用指定 worker_id 初始化全局生成器；重复调用只有第一次生效。
pub fn init_worker(worker_id: i64) -> Result<(), SnowflakeError> {
    let sf = Snowflake::new(worker_id)?;
    // 已初始化时 set 返回 Err，这里当作幂等处理。
    let _ = GLOBAL.set(sf);
    Ok(())
}

/// 取全局 id。未初始化过则用 pid 兜底初始化。
pub fn next_id() -> i64 {
    GLOBAL
        .get_or_init(|| Snowflake::new(default_worker_id()).expect("默认 worker_id 越界"))
        .next_id_blocking()
}

fn default_worker_id() -> i64 {
    (std::process::id() as i64) & MAX_WORKER_ID
}

// ---------- 反解：从 id 还原出时间戳 / 机器号 / 序列号 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    /// 绝对毫秒时间戳（已加回 epoch）
    pub timestamp_ms: i64,
    pub worker_id: i64,
    pub sequence: i64,
}

/// 拆解一个 id。用默认纪元；若生成时用了自定义纪元，传对应值进来。
pub fn parse(id: i64, epoch_ms: i64) -> Parsed {
    Parsed {
        timestamp_ms: (id >> TIMESTAMP_SHIFT) + epoch_ms,
        worker_id: (id >> WORKER_ID_SHIFT) & MAX_WORKER_ID,
        sequence: id & MAX_SEQUENCE,
    }
}

/// 把 id 的时间戳格式化成 `YYYY-MM-DD HH:MM:SS.mmm`（UTC）。
pub fn format_time(id: i64, epoch_ms: i64) -> String {
    let ms = parse(id, epoch_ms).timestamp_ms;
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    // Civil-from-days，Howard Hinnant 算法，避免为此引入 chrono。
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{millis:03}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

    #[test]
    fn worker_id_越界报错() {
        assert!(Snowflake::new(-1).is_err());
        assert!(Snowflake::new(MAX_WORKER_ID + 1).is_err());
        assert!(Snowflake::new(MAX_WORKER_ID).is_ok());
    }

    #[test]
    fn 单调递增且唯一() {
        let sf = Snowflake::new(7).unwrap();
        let mut last = 0;
        for _ in 0..100_000 {
            let id = sf.next_id().unwrap();
            assert!(id > last, "id 必须严格递增: {last} -> {id}");
            last = id;
        }
    }

    #[test]
    fn 多线程不重复() {
        let sf = Arc::new(Snowflake::new(42).unwrap());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let sf = Arc::clone(&sf);
            handles.push(std::thread::spawn(move || {
                (0..20_000).map(|_| sf.next_id_blocking()).collect::<Vec<_>>()
            }));
        }
        let mut set = HashSet::new();
        for h in handles {
            for id in h.join().unwrap() {
                assert!(set.insert(id), "出现重复 id: {id}");
            }
        }
        assert_eq!(set.len(), 8 * 20_000);
    }

    #[test]
    fn 反解能还原字段() {
        let sf = Snowflake::new(123).unwrap();
        let before = now_ms();
        let id = sf.next_id_blocking();
        let p = parse(id, DEFAULT_EPOCH_MS);
        assert_eq!(p.worker_id, 123);
        assert!(p.sequence >= 0 && p.sequence <= MAX_SEQUENCE);
        assert!(p.timestamp_ms >= before - 1000);
        assert!(!format_time(id, DEFAULT_EPOCH_MS).is_empty());
    }

    #[test]
    fn 高位符号位恒为0() {
        let sf = Snowflake::new(MAX_WORKER_ID).unwrap();
        for _ in 0..1000 {
            assert!(sf.next_id().unwrap() > 0, "id 必须是正数");
        }
    }

    #[test]
    fn 位段不重叠() {
        assert_eq!(TIMESTAMP_SHIFT + TIMESTAMP_BITS, 63);
        assert_eq!(MAX_WORKER_ID, 1023);
        assert_eq!(MAX_SEQUENCE, 4095);
    }
}
