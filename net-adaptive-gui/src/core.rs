// ════════════════════════════════════════════════════════════
//  网络测速引擎核心 (供 Slint GUI 调用)
//  纯逻辑: 返回结构化数据, 不做任何终端打印; 长任务通过回调上报进度
//  运行时仍调用系统命令: ping / ethtool / ip / arp / ifconfig / sudo
// ════════════════════════════════════════════════════════════

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// ───────────── 可配置参数 ─────────────
// sudo 密码: 从环境变量 NET_TEST_SUDO_PASS 读取 (默认空)。
// 直接用 `sudo ./net-adaptive-gui` 运行时无需设置 —— 已是 root, sudo 不再要密码。
pub fn sudo_pass() -> String {
    std::env::var("NET_TEST_SUDO_PASS").unwrap_or_default()
}
pub const SETTLE_SEC: u32 = 5; // 切档后等待协商稳定的秒数
pub const PING_W: u32 = 2; // 单包超时秒数 (MTU 探测等)
pub const SCAN_PING_W: u32 = 1; // 局域网发现单包超时
pub const SCAN_FANOUT: usize = 64; // 主机发现并发线程数

// ───────────── 数据结构 ─────────────
#[derive(Clone, Debug)]
pub struct Nic {
    pub name: String,
    pub state: String,
    pub physical: bool,
    pub ipv4: Option<String>, // "192.168.1.5/24"
    pub mtu: u32,
}

#[derive(Clone, Debug)]
pub struct Host {
    pub ip: String,
    pub mac: String,
    pub is_self: bool,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub reachable: bool,
    pub loss: Option<f64>,
    pub avg: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct MtuResult {
    pub path_mtu: i64,
    pub link_mtu: i64,
    pub full: bool, // 路径 MTU >= 网卡 MTU
    pub skipped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
    NoReply,
    Error,
}

#[derive(Clone, Debug)]
pub struct Tier {
    pub label: String,
    pub speed: String,
    pub duplex: String,
    pub link: bool,
    pub loss: Option<f64>,
    pub min: Option<f64>,
    pub avg: Option<f64>,
    pub max: Option<f64>,
    pub jit: Option<f64>,
    pub d_err: i64,
    pub d_drop: i64,
    pub d_crc: i64,
    pub d_col: i64,
    pub errn: i64,
    pub verdict: Verdict,
}

// ───────────── 特权命令 ─────────────
pub fn run_sudo(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut child = Command::new("sudo")
        .arg("-S")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(format!("{}\n", sudo_pass()).as_bytes());
    }
    child.wait_with_output()
}

pub fn cache_sudo() -> bool {
    run_sudo(&["-v"]).map(|o| o.status.success()).unwrap_or(false)
}

pub fn restore_nic(nic: &str) {
    if !nic.is_empty() {
        let _ = run_sudo(&["ethtool", "-s", nic, "autoneg", "on"]);
    }
}

// ───────────── ping 封装与解析 ─────────────
fn ping_capture(args: &[&str]) -> String {
    match Command::new("ping").args(args).env("LC_ALL", "C").output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        }
        Err(_) => String::new(),
    }
}
fn ping_ok(args: &[&str]) -> bool {
    Command::new("ping")
        .args(args)
        .env("LC_ALL", "C")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn parse_loss(out: &str) -> Option<f64> {
    let idx = out.find("% packet loss")?;
    let pre = &out[..idx];
    let rev: String = pre
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    rev.chars().rev().collect::<String>().parse().ok()
}
// rtt min/avg/max/mdev = 0.12/0.45/0.89/0.11 ms
pub fn parse_rtt(out: &str) -> Option<(f64, f64, f64, f64)> {
    let line = out
        .lines()
        .find(|l| l.contains("rtt ") || l.contains("round-trip"))?;
    let after = line.split(" = ").nth(1)?;
    let mut it = after.trim().split('/');
    let min = it.next()?.trim().parse().ok()?;
    let avg = it.next()?.trim().parse().ok()?;
    let max = it.next()?.trim().parse().ok()?;
    let mdev = it.next()?.split_whitespace().next()?.parse().ok()?;
    Some((min, avg, max, mdev))
}

// ───────────── sysfs ─────────────
fn read_sysfs(nic: &str, sub: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/class/net/{}/{}", nic, sub))
        .ok()
        .map(|s| s.trim().to_string())
}
pub fn nic_stat(nic: &str, key: &str) -> i64 {
    read_sysfs(nic, &format!("statistics/{}", key))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ───────────── 网卡枚举 ─────────────
pub fn list_nics(show_all: bool) -> Vec<Nic> {
    let mut names: Vec<String> = vec![];
    if let Ok(rd) = fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n != "lo" {
                names.push(n);
            }
        }
    }
    names.sort();

    let is_phys = |n: &str| std::path::Path::new(&format!("/sys/class/net/{}/device", n)).exists();
    let mut chosen: Vec<String> = if show_all {
        names.clone()
    } else {
        names.iter().filter(|n| is_phys(n)).cloned().collect()
    };
    if chosen.is_empty() {
        chosen = names;
    }

    chosen
        .into_iter()
        .map(|n| Nic {
            state: read_sysfs(&n, "operstate").unwrap_or_else(|| "unknown".into()),
            physical: is_phys(&n),
            ipv4: nic_ipv4(&n),
            mtu: read_sysfs(&n, "mtu").and_then(|s| s.parse().ok()).unwrap_or(1500),
            name: n,
        })
        .collect()
}

pub fn nic_ipv4(nic: &str) -> Option<String> {
    if let Ok(o) = Command::new("ip")
        .args(["-o", "-4", "addr", "show", "dev", nic])
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = s.lines().next() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 4 && f[3].contains('/') {
                return Some(f[3].to_string());
            }
        }
    }
    if let Ok(o) = Command::new("ifconfig").arg(nic).output() {
        let s = String::from_utf8_lossy(&o.stdout);
        for line in s.lines() {
            if let Some(pos) = line.find("inet ") {
                let rest = &line[pos + 5..];
                let ip = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("addr:");
                if !ip.is_empty() {
                    return Some(format!("{}/24", ip));
                }
            }
        }
    }
    None
}

// ───────────── 局域网主机发现 ─────────────
fn neighbor_macs() -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut m = HashMap::new();
    if let Ok(o) = Command::new("ip").arg("neigh").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if let Some(pos) = f.iter().position(|x| *x == "lladdr") {
                if pos + 1 < f.len() {
                    m.insert(f[0].to_string(), f[pos + 1].to_string());
                }
            }
        }
    }
    if let Ok(o) = Command::new("arp").arg("-n").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 3 && f[2].contains(':') {
                m.entry(f[0].to_string()).or_insert_with(|| f[2].to_string());
            }
        }
    }
    m
}

// progress(done, total)
pub fn scan_hosts<F: Fn(u32, u32)>(nic: &str, progress: F) -> Result<Vec<Host>, String> {
    let cidr = nic_ipv4(nic).ok_or_else(|| format!("网卡 {} 未配置 IPv4, 无法推断网段", nic))?;
    let ip4 = cidr.split('/').next().unwrap_or("").to_string();
    let base = ip4
        .rsplit_once('.')
        .map(|(b, _)| b.to_string())
        .ok_or_else(|| "IPv4 解析失败".to_string())?;

    let alive = Arc::new(Mutex::new(Vec::<u8>::new()));
    let next = Arc::new(AtomicUsize::new(1));
    let done = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for _ in 0..SCAN_FANOUT {
        let alive = Arc::clone(&alive);
        let next = Arc::clone(&next);
        let done = Arc::clone(&done);
        let base = base.clone();
        handles.push(thread::spawn(move || loop {
            let h = next.fetch_add(1, Ordering::Relaxed);
            if h > 254 {
                break;
            }
            let ip = format!("{}.{}", base, h);
            if ping_ok(&["-c", "1", "-W", &SCAN_PING_W.to_string(), &ip]) {
                alive.lock().unwrap().push(h as u8);
            }
            done.fetch_add(1, Ordering::Relaxed);
        }));
    }
    // 主线程轮询进度
    loop {
        let d = done.load(Ordering::Relaxed);
        progress(d.min(254) as u32, 254);
        if d >= 254 {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(120));
    }
    for h in handles {
        let _ = h.join();
    }

    let mut alive = Arc::try_unwrap(alive).unwrap().into_inner().unwrap();
    alive.sort_unstable();
    let macs = neighbor_macs();
    Ok(alive
        .iter()
        .map(|h| {
            let ip = format!("{}.{}", base, h);
            Host {
                mac: macs.get(&ip).cloned().unwrap_or_else(|| "—".into()),
                is_self: ip == ip4,
                ip,
            }
        })
        .collect())
}

// ───────────── 连通性预检 ─────────────
pub fn connectivity(target: &str, seconds: u32) -> Check {
    let out = ping_capture(&[
        "-c",
        &seconds.to_string(),
        "-i",
        "1",
        "-w",
        &(seconds + 2).to_string(),
        target,
    ]);
    let loss = parse_loss(&out);
    let avg = parse_rtt(&out).map(|(_, a, _, _)| a);
    let reachable = !(loss.is_none() || matches!(loss, Some(l) if l >= 100.0));
    Check { reachable, loss, avg }
}

// ───────────── MTU 探测 ─────────────
pub fn mtu_probe(nic: &str, target: &str) -> MtuResult {
    let link_mtu: i64 = read_sysfs(nic, "mtu")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500);
    let w = PING_W.to_string();
    if !ping_ok(&["-c", "1", "-W", &w, "-M", "do", "-s", "1", target]) {
        return MtuResult { path_mtu: 0, link_mtu, full: false, skipped: true };
    }
    let (mut lo, mut hi, mut found) = (1i64, link_mtu - 28, 1i64);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        if ping_ok(&["-c", "1", "-W", &w, "-M", "do", "-s", &mid.to_string(), target]) {
            found = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    let path_mtu = found + 28;
    MtuResult { path_mtu, link_mtu, full: path_mtu >= link_mtu, skipped: false }
}

// ───────────── 切档 & 读链路 ─────────────
pub fn set_tier(nic: &str, ethtool_args: &[&str]) -> bool {
    let mut cmd: Vec<&str> = vec!["ethtool", "-s", nic];
    cmd.extend_from_slice(ethtool_args);
    run_sudo(&cmd).map(|o| o.status.success()).unwrap_or(false)
}

pub fn read_link(nic: &str) -> (String, String, bool) {
    let (mut speed, mut duplex, mut link) = (String::from("—"), String::from("—"), false);
    if let Ok(o) = Command::new("ethtool").arg(nic).env("LC_ALL", "C").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("Speed:") {
                speed = v.trim().to_string();
            } else if let Some(v) = l.strip_prefix("Duplex:") {
                duplex = v.trim().to_string();
            } else if let Some(v) = l.strip_prefix("Link detected:") {
                link = v.trim() == "yes";
            }
        }
    }
    (speed, duplex, link)
}

// ───────────── 单档发包测试 (设置协商由调用方先做; 此处只发包+统计) ─────────────
pub fn run_ping_tier(nic: &str, target: &str, count: u32, do_stats: bool) -> Tier {
    let (speed, duplex, link) = read_link(nic);

    let base = if do_stats {
        (
            nic_stat(nic, "rx_errors"),
            nic_stat(nic, "tx_errors"),
            nic_stat(nic, "rx_dropped"),
            nic_stat(nic, "tx_dropped"),
            nic_stat(nic, "rx_crc_errors"),
            nic_stat(nic, "collisions"),
        )
    } else {
        (0, 0, 0, 0, 0, 0)
    };

    let out = ping_capture(&["-c", &count.to_string(), target]);
    let loss = parse_loss(&out);
    let rtt = parse_rtt(&out);

    let (mut d_err, mut d_drop, mut d_crc, mut d_col) = (0i64, 0i64, 0i64, 0i64);
    if do_stats {
        d_err = (nic_stat(nic, "rx_errors") - base.0) + (nic_stat(nic, "tx_errors") - base.1);
        d_drop = (nic_stat(nic, "rx_dropped") - base.2) + (nic_stat(nic, "tx_dropped") - base.3);
        d_crc = nic_stat(nic, "rx_crc_errors") - base.4;
        d_col = nic_stat(nic, "collisions") - base.5;
        d_err = d_err.max(0);
        d_drop = d_drop.max(0);
        d_crc = d_crc.max(0);
        d_col = d_col.max(0);
    }
    let errn = d_err + d_drop + d_crc + d_col;

    let verdict = match loss {
        None => Verdict::NoReply,
        Some(l) if l == 0.0 => {
            if errn > 0 {
                Verdict::Warn
            } else {
                Verdict::Pass
            }
        }
        Some(l) if l >= 100.0 => Verdict::Fail,
        Some(_) => Verdict::Warn,
    };

    let (min, avg, max, jit) = match rtt {
        Some((mi, a, ma, j)) => (Some(mi), Some(a), Some(ma), Some(j)),
        None => (None, None, None, None),
    };

    Tier {
        label: String::new(),
        speed,
        duplex,
        link,
        loss,
        min,
        avg,
        max,
        jit,
        d_err,
        d_drop,
        d_crc,
        d_col,
        errn,
        verdict,
    }
}

// 便于展示的数字格式化
pub fn fmt_num(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        format!("{:.3}", v)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}
pub fn fmt_opt(v: Option<f64>) -> String {
    v.map(fmt_num).unwrap_or_else(|| "—".into())
}
