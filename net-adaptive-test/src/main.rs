// ════════════════════════════════════════════════════════════
//  NET ADAPTIVE TEST · 网卡自适应测速  (Rust 端口, 目标 aarch64-linux)
//  链路协商 100M / 10M / 1000M · 丢包 · 延迟 · MTU · 判定
// ════════════════════════════════════════════════════════════
//  由 自适应网络测试_beta.sh 重写。相比原 Bash 版:
//   · 局域网扫描用线程池 (SCAN_FANOUT 个 worker) 取代一次性 fork 254 个子进程
//   · ping/ethtool 输出解析用原生 Rust, 不再依赖 grep/awk/sed
//   · 单一静态二进制, 无需 bash/awk/mktemp, 适合精简 ARM 系统
//  仍调用系统命令: ping / ethtool / ip / arp / ifconfig / sudo / stty
// ════════════════════════════════════════════════════════════

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

// ───────────── 可配置参数 ─────────────
// sudo 密码: 从环境变量 NET_TEST_SUDO_PASS 读取 (默认空)。
// 直接用 `sudo ./net-adaptive-test` 运行时无需设置 —— 已是 root, sudo 不再要密码。
fn sudo_pass() -> String {
    std::env::var("NET_TEST_SUDO_PASS").unwrap_or_default()
}
const PING_COUNT: u32 = 10; // 每档发包数
const CHECK_SEC: u32 = 5; // 启动连通性预检的 Ping 秒数
const SETTLE_SEC: u32 = 5; // 切档后等待协商稳定的秒数
const PING_W: u32 = 2; // 单包超时秒数 (MTU 探测等用)
const DO_NIC_STATS: bool = true; // 每档读取网卡错误/丢弃/CRC/冲突计数器
const DO_MTU_PROBE: bool = true; // 每轮 DF 大包二分探测路径 MTU
const DO_CHART: bool = true; // 汇总后绘制各档平均延迟折线图
const SCAN_PING_W: u32 = 1; // 局域网主机发现单包超时秒数
const SCAN_FANOUT: usize = 64; // 主机发现并发线程上限
const W: usize = 60; // 界面宽度

// 当前网卡 (供 Ctrl+C 处理器恢复自动协商)
static CURRENT_NIC: Mutex<Option<String>> = Mutex::new(None);
static THEME: OnceLock<Theme> = OnceLock::new();

// ───────────── 配色 ─────────────
#[derive(Clone, Copy)]
struct Theme {
    b: &'static str,
    d: &'static str,
    r: &'static str,
    red: &'static str,
    grn: &'static str,
    ylw: &'static str,
    cyn: &'static str,
    gry: &'static str,
}
impl Theme {
    fn new(color: bool) -> Self {
        if color {
            Theme {
                b: "\x1b[1m",
                d: "\x1b[2m",
                r: "\x1b[0m",
                red: "\x1b[31m",
                grn: "\x1b[32m",
                ylw: "\x1b[33m",
                cyn: "\x1b[36m",
                gry: "\x1b[90m",
            }
        } else {
            Theme { b: "", d: "", r: "", red: "", grn: "", ylw: "", cyn: "", gry: "" }
        }
    }
}
fn t() -> &'static Theme {
    THEME.get().expect("theme not initialised")
}
fn flush() {
    let _ = io::stdout().flush();
}

// ───────────── 绘制函数 ─────────────
fn rule(ch: &str) {
    let th = t();
    println!("{}{}{}", th.gry, ch.repeat(W), th.r);
}
fn title(s: &str) {
    let th = t();
    println!();
    println!("{}{}  {}{}", th.cyn, th.b, s, th.r);
    rule("─");
}
fn note(s: &str) {
    let th = t();
    println!("  {}{}{}", th.gry, s, th.r);
}
fn die(s: &str) -> ! {
    let th = t();
    eprintln!("  {}✗ {}{}", th.red, s, th.r);
    std::process::exit(1);
}
fn clear() {
    if io::stdout().is_terminal() {
        print!("\x1b[2J\x1b[H");
        flush();
    }
}

fn banner() {
    let th = t();
    clear();
    rule("═");
    println!(
        "  {}{}NET ADAPTIVE TEST{}  {}·{}  {}{}网卡自适应测速{}",
        th.cyn, th.b, th.r, th.d, th.r, th.cyn, th.b, th.r
    );
    println!("  {}链路协商 100M / 10M / 1000M · 丢包 · 延迟 · 判定{}", th.d, th.r);
    rule("═");
}

// ───────────── 开机自检动画 (启动时放一次) ─────────────
fn boot() {
    let th = t();
    if !io::stdout().is_terminal() {
        banner();
        return;
    }
    clear();
    rule("═");
    println!("  {}{}NET ADAPTIVE TEST{}", th.cyn, th.b, th.r);
    thread::sleep(Duration::from_millis(120));
    println!("  {}网卡自适应测速 · 链路诊断引擎{}", th.d, th.r);
    thread::sleep(Duration::from_millis(120));
    rule("═");
    thread::sleep(Duration::from_millis(100));

    let steps = ["加载配置", "探测网络接口", "校验 ethtool / ping", "初始化测速引擎", "缓存 sudo 凭据"];
    for s in steps {
        print!("  {}…{} {}{}{}\r", th.d, th.r, th.d, s, th.r);
        flush();
        thread::sleep(Duration::from_millis(160));
        println!("  {}✓{} {}        ", th.grn, th.r, s);
        thread::sleep(Duration::from_millis(50));
    }
    println!();

    let width = 36usize;
    for i in 0..=width {
        let bar = "█".repeat(i);
        print!(
            "\r  {}启动中{}  {}{:<width$}{}  {}{:>3}%{}",
            th.d,
            th.r,
            th.cyn,
            bar,
            th.r,
            th.b,
            i * 100 / width,
            th.r,
            width = width
        );
        flush();
        thread::sleep(Duration::from_millis(18));
    }
    println!("   {}● 就绪{}", th.grn, th.r);
    thread::sleep(Duration::from_millis(350));
}

// ───────────── 工具函数 ─────────────
fn valid_ip(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    for p in parts {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if p.parse::<u32>().unwrap_or(999) > 255 {
            return false;
        }
    }
    true
}

fn has_cmd(c: &str) -> bool {
    if let Ok(path) = env::var("PATH") {
        for dir in path.split(':') {
            if Path::new(dir).join(c).exists() {
                return true;
            }
        }
    }
    false
}

// 以 sudo -S 执行特权命令, 从 stdin 喂密码
fn run_sudo(args: &[&str]) -> io::Result<std::process::Output> {
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

fn cache_sudo() {
    match run_sudo(&["-v"]) {
        Ok(o) if o.status.success() => {}
        _ => note("sudo 凭据缓存失败 (密码可能不正确), 切档/恢复网卡时可能需要手动授权"),
    }
}

// 读取网卡某项统计计数 (sysfs, 世界可读, 不存在则记 0)
fn nic_stat(nic: &str, key: &str) -> i64 {
    fs::read_to_string(format!("/sys/class/net/{}/statistics/{}", nic, key))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn conf_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".net_speed_test.conf")
}
fn load_conf() -> String {
    fs::read_to_string(conf_path())
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .unwrap_or_default()
}
fn save_conf(ip: &str) {
    let _ = fs::write(conf_path(), ip);
}

fn preflight() {
    let th = t();
    for c in ["ethtool", "ping", "ifconfig"] {
        if !has_cmd(c) {
            note(&format!("缺少 {}{}{}{}, 相关功能可能不可用", th.b, c, th.r, th.gry));
        }
    }
}

fn read_line() -> String {
    let mut s = String::new();
    let _ = io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

// 协商稳定倒计时 (原地刷新)
fn settle(mut s: u32) {
    let th = t();
    while s > 0 {
        print!("\r  {}链路协商稳定中… {}s {}", th.d, s, th.r);
        flush();
        thread::sleep(Duration::from_secs(1));
        s -= 1;
    }
    println!("\r  {}● 链路就绪{}            ", th.grn, th.r);
}

// 显示宽度 (CJK 记 2), 用于对齐汇总表
fn dwidth(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            let wide = (0x1100..=0x115F).contains(&u)
                || (0x2E80..=0xA4CF).contains(&u)
                || (0xAC00..=0xD7A3).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFE30..=0xFE4F).contains(&u)
                || (0xFF00..=0xFF60).contains(&u)
                || (0xFFE0..=0xFFE6).contains(&u);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}
fn padr(s: &str, w: usize) -> String {
    let dw = dwidth(s);
    if dw >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - dw))
    }
}

// ───────────── ping 输出解析 ─────────────
fn parse_loss(out: &str) -> Option<f64> {
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
fn parse_rtt(out: &str) -> Option<(f64, f64, f64, f64)> {
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
fn fmt_num(v: f64) -> String {
    if (v.fract()).abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        // 去掉多余尾零
        let s = format!("{:.3}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// 运行一次 ping, 返回合并的 stdout+stderr
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

// ───────────── 选择网卡 ─────────────
fn list_all_nics() -> Vec<String> {
    let mut v = vec![];
    if let Ok(rd) = fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name != "lo" {
                v.push(name);
            }
        }
    }
    v.sort();
    v
}
fn is_physical(nic: &str) -> bool {
    Path::new(&format!("/sys/class/net/{}/device", nic)).exists()
}
fn operstate(nic: &str) -> String {
    fs::read_to_string(format!("/sys/class/net/{}/operstate", nic))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn build_nic_list(show_all: bool) -> Vec<String> {
    let all = list_all_nics();
    let mut nics: Vec<String> = if show_all {
        all.clone()
    } else {
        all.iter().filter(|n| is_physical(n)).cloned().collect()
    };
    if nics.is_empty() {
        nics = all; // 兜底: 没物理口就全列
    }
    if nics.is_empty() {
        die("未检测到可用网卡");
    }
    nics
}

fn render_nic_list(nics: &[String], show_all: bool) {
    let th = t();
    title("选择网卡");
    if show_all {
        note("已显示全部接口 (含虚拟)");
    } else {
        note("仅物理网卡 · 输 a 显示全部接口");
    }
    for (i, nm) in nics.iter().enumerate() {
        let st = operstate(nm);
        let dot = if st == "up" {
            format!("{}●{}", th.grn, th.r)
        } else {
            format!("{}●{}", th.gry, th.r)
        };
        let stshow = if st.is_empty() { "unknown".to_string() } else { st };
        println!(
            "  {}{:>2}{}  {}  {}  {}{}{}",
            th.b,
            i + 1,
            th.r,
            dot,
            padr(nm, 16),
            th.d,
            stshow,
            th.r
        );
    }
    println!();
}

fn choose_nic(show_all: &mut bool) -> String {
    let th = t();
    let mut render = true;
    let mut nics: Vec<String> = vec![];
    loop {
        if render {
            nics = build_nic_list(*show_all);
            render_nic_list(&nics, *show_all);
            render = false;
        }
        print!("  {}❯{} 网卡序号 {}(a=全部){}: ", th.cyn, th.r, th.d, th.r);
        flush();
        let sel = read_line();
        match sel.as_str() {
            "a" | "A" => {
                *show_all = true;
                banner();
                render = true;
                continue;
            }
            "p" | "P" => {
                *show_all = false;
                banner();
                render = true;
                continue;
            }
            _ => {}
        }
        if let Ok(n) = sel.parse::<usize>() {
            if n >= 1 && n <= nics.len() {
                return nics[n - 1].clone();
            }
        }
        println!("  {}! 无效输入, 请输序号或 a{}", th.ylw, th.r);
    }
}

// ───────────── 扫描对方 IP (局域网主机发现) ─────────────
fn nic_ipv4(nic: &str) -> Option<String> {
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
    // 兜底: ifconfig
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

// 邻居表 (ip neigh + arp) -> IP:MAC
fn neighbor_macs() -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(o) = Command::new("ip").arg("neigh").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if let Some(pos) = f.iter().position(|x| *x == "lladdr") {
                if pos + 1 < f.len() && !f.is_empty() {
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

// 返回 (ip, mac) 列表, 已按末段排序
fn scan_hosts(nic: &str) -> Option<Vec<(String, String)>> {
    let th = t();
    title("扫描对方 IP");
    let cidr = match nic_ipv4(nic) {
        Some(c) => c,
        None => {
            println!("  {}! 网卡 {}{}{}{} 未配置 IPv4, 无法推断网段{}", th.ylw, th.b, nic, th.r, th.ylw, th.r);
            note("可先给网卡设个同网段 IP, 或直接手动输入目标");
            return None;
        }
    };
    let ip4 = cidr.split('/').next().unwrap_or("").to_string();
    let prefix = cidr.split('/').nth(1).unwrap_or("24").to_string();
    let base = match ip4.rsplit_once('.') {
        Some((b, _)) => b.to_string(),
        None => return None,
    };
    note(&format!(
        "本机 {}{}/{}{}{}, 并发 Ping 扫描 {}{}.1-254{}{} 存活主机",
        th.b, ip4, prefix, th.r, th.gry, th.b, base, th.r, th.gry
    ));
    if prefix != "24" {
        note(&format!("网段非 /24, 仅扫描本机所在的 {}.0/24 末段", base));
    }

    // 线程池并发 ping (取代 fork 254 个子进程)
    let alive = Arc::new(Mutex::new(Vec::<u8>::new()));
    let next = Arc::new(AtomicUsize::new(1));
    let base_a = base.clone();
    print!("  {}扫描中… (并发 {}){}", th.d, SCAN_FANOUT, th.r);
    flush();

    let mut handles = vec![];
    for _ in 0..SCAN_FANOUT {
        let alive = Arc::clone(&alive);
        let next = Arc::clone(&next);
        let base = base_a.clone();
        handles.push(thread::spawn(move || loop {
            let h = next.fetch_add(1, Ordering::Relaxed);
            if h > 254 {
                break;
            }
            let ip = format!("{}.{}", base, h);
            if ping_ok(&["-c", "1", "-W", &SCAN_PING_W.to_string(), &ip]) {
                alive.lock().unwrap().push(h as u8);
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    print!("\r                                        \r");
    flush();

    let mut alive = Arc::try_unwrap(alive).unwrap().into_inner().unwrap();
    alive.sort_unstable();

    let macs = neighbor_macs();
    let result: Vec<(String, String)> = alive
        .iter()
        .map(|h| {
            let ip = format!("{}.{}", base, h);
            let mac = macs.get(&ip).cloned().unwrap_or_else(|| "—".to_string());
            (ip, mac)
        })
        .collect();

    if result.is_empty() {
        println!("  {}! 未发现存活主机 (对方可能屏蔽 ICMP, 可手动输入目标){}", th.ylw, th.r);
        return None;
    }
    println!("  {}● 发现 {} 台主机{}", th.grn, result.len(), th.r);
    for (i, (dip, dmac)) in result.iter().enumerate() {
        let tag = if *dip == ip4 {
            format!("{}(本机){}", th.d, th.r)
        } else {
            String::new()
        };
        println!(
            "  {}{:>2}{}  {}▸{} {}  {}{}{} {}",
            th.b,
            i + 1,
            th.r,
            th.grn,
            th.r,
            padr(dip, 15),
            th.d,
            padr(dmac, 17),
            th.r,
            tag
        );
    }
    println!();
    Some(result)
}

// ───────────── 选择目标 IP (回车沿用 · s=扫描局域网) ─────────────
fn choose_target(nic: &str, last_ip: &mut String) -> String {
    let th = t();
    title("目标地址");
    loop {
        let mut ip;
        if !last_ip.is_empty() {
            print!("  {}❯{} 目标 IP {}[回车={} · s=扫描局域网]{}: ", th.cyn, th.r, th.d, last_ip, th.r);
            flush();
            ip = read_line();
            if ip.is_empty() {
                ip = last_ip.clone();
            }
        } else {
            print!("  {}❯{} 目标 IP {}(s=扫描局域网发现对方 IP){}: ", th.cyn, th.r, th.d, th.r);
            flush();
            ip = read_line();
        }
        // s = 扫描同网段, 再从结果里选一个作为目标
        if ip == "s" || ip == "S" {
            let hosts = match scan_hosts(nic) {
                Some(h) => h,
                None => continue,
            };
            print!("  {}❯{} 选序号设为目标 {}(回车=重新输入){}: ", th.cyn, th.r, th.d, th.r);
            flush();
            let sel = read_line();
            match sel.parse::<usize>() {
                Ok(n) if n >= 1 && n <= hosts.len() => ip = hosts[n - 1].0.clone(),
                _ => continue,
            }
        }
        if valid_ip(&ip) {
            *last_ip = ip.clone();
            save_conf(&ip);
            return ip;
        }
        println!("  {}! IP 格式不正确, 请重新输入{}", th.ylw, th.r);
    }
}

// ───────────── 启动连通性预检 ─────────────
// 返回 (是否继续本轮, 预检摘要)
fn connectivity_check(target: &str) -> (bool, String) {
    let th = t();
    title("连通性预检");
    note(&format!("启动测速前向目标 Ping {}{}{}{} 秒, 确认链路可达", th.b, CHECK_SEC, th.r, th.gry));

    // 后台持续 ping, 前台原地刷新倒计时
    let child = Command::new("ping")
        .args([
            "-c",
            &CHECK_SEC.to_string(),
            "-i",
            "1",
            "-w",
            &(CHECK_SEC + 2).to_string(),
            target,
        ])
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let out = match child {
        Ok(mut c) => {
            let mut s = CHECK_SEC;
            while s > 0 {
                if let Ok(Some(_)) = c.try_wait() {
                    break;
                }
                print!("\r  {}❯{} 正在 Ping {}{}{} … 剩余 {}{}s{}   ", th.cyn, th.r, th.b, target, th.r, th.ylw, s, th.r);
                flush();
                thread::sleep(Duration::from_secs(1));
                s -= 1;
            }
            let o = c.wait_with_output().ok();
            o.map(|o| {
                let mut txt = String::from_utf8_lossy(&o.stdout).into_owned();
                txt.push_str(&String::from_utf8_lossy(&o.stderr));
                txt
            })
            .unwrap_or_default()
        }
        Err(_) => String::new(),
    };

    let loss = parse_loss(&out);
    let avg = parse_rtt(&out).map(|(_, a, _, _)| a);
    let avg_s = avg.map(fmt_num).unwrap_or_else(|| "—".to_string());

    let unreachable = matches!(loss, None) || matches!(loss, Some(l) if l >= 100.0);
    if unreachable {
        println!("\r  {}✗ 目标不可达 (100% 丢包 / 无响应){}                         ", th.red, th.r);
        let prechk = format!("{}不可达{}", th.red, th.r);
        print!("  {}! 链路不通, 仍要继续逐档测试吗? (y/N): {}", th.ylw, th.r);
        flush();
        let ans = read_line();
        if !(ans == "y" || ans == "Y") {
            note("已取消本轮, 返回重新选择");
            return (false, prechk);
        }
        (true, prechk)
    } else {
        let loss_s = fmt_num(loss.unwrap_or(0.0));
        println!(
            "\r  {}● 链路可达{}   {}丢包{} {}{}%{}   {}平均延迟{} {}{} ms{}        ",
            th.grn, th.r, th.d, th.r, th.b, loss_s, th.r, th.d, th.r, th.b, avg_s, th.r
        );
        let prechk = format!("{}可达{} {}(丢包 {}% · {} ms){}", th.grn, th.r, th.d, loss_s, avg_s, th.r);
        (true, prechk)
    }
}

// ───────────── MTU / 大包探测 ─────────────
fn mtu_probe(nic: &str, target: &str) -> String {
    let th = t();
    if !DO_MTU_PROBE {
        return String::new();
    }
    title("MTU / 大包探测");
    let linkmtu: i64 = fs::read_to_string(format!("/sys/class/net/{}/mtu", nic))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1500);
    let lo0 = 1i64;
    let hi0 = linkmtu - 28; // IPv4: payload 上限 = MTU - 20(IP) - 8(ICMP)
    note(&format!(
        "网卡 MTU={}{}{}{}, 以 DF(禁分片) 二分探测可通过的最大包 (约数秒)",
        th.b, linkmtu, th.r, th.gry
    ));

    let w = PING_W.to_string();
    // 先确认 DF 小包能通
    if !ping_ok(&["-c", "1", "-W", &w, "-M", "do", "-s", &lo0.to_string(), target]) {
        note("DF 小包不通, 跳过 (目标可能屏蔽 ICMP / 不支持 DF)");
        return "—".to_string();
    }

    let (mut lo, mut hi, mut found) = (lo0, hi0, lo0);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        print!("\r  {}探测中… payload={:<5}{}", th.d, mid, th.r);
        flush();
        if ping_ok(&["-c", "1", "-W", &w, "-M", "do", "-s", &mid.to_string(), target]) {
            found = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    print!("\r                                   \r");
    flush();

    let pathmtu = found + 28;
    if pathmtu >= linkmtu {
        println!("  {}● 路径 MTU = {}{}  {}(= 网卡 MTU, 大包直通, 无需分片){}", th.grn, pathmtu, th.r, th.d, th.r);
        pathmtu.to_string()
    } else {
        println!(
            "  {}! 路径 MTU = {}{}  {}(< 网卡 MTU {}, 存在分片/黑洞风险){}",
            th.ylw, pathmtu, th.r, th.d, linkmtu, th.r
        );
        format!("{} (< {})", pathmtu, linkmtu)
    }
}

// ───────────── 单档测试 ─────────────
struct Row {
    label: String,
    loss: String,   // "0%" / "—%"
    avg: String,    // "0.45 ms" / "— ms"
    jit: String,    // "0.11 ms" / "— ms"
    errn: i64,
    verdict: String, // 含颜色码
}

fn run_test(label: &str, nic: &str, target: &str, ethtool_args: &[&str], rows: &mut Vec<Row>) {
    let th = t();
    title(&format!("测试 {}", label));

    // sudo ethtool -s NIC <args>
    let mut cmd: Vec<&str> = vec!["ethtool", "-s", nic];
    cmd.extend_from_slice(ethtool_args);
    let ok = run_sudo(&cmd).map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        println!("  {}✗ 网卡设置失败, 跳过本档{}", th.red, th.r);
        rows.push(Row {
            label: label.to_string(),
            loss: "—".into(),
            avg: "—".into(),
            jit: "—".into(),
            errn: -1,
            verdict: format!("{}ERROR{}", th.red, th.r),
        });
        return;
    }
    settle(SETTLE_SEC);

    // 链路信息
    if let Ok(o) = Command::new("ethtool").arg(nic).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if line.contains("Speed") || line.contains("Duplex") || line.contains("Link detected") {
                println!("  {}{}{}", th.gry, line.trim(), th.r);
            }
        }
    }

    // 计数器基线
    let (re0, te0, rd0, td0, crc0, col0) = if DO_NIC_STATS {
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

    let out = ping_capture(&["-c", &PING_COUNT.to_string(), target]);
    let loss = parse_loss(&out);
    let rtt = parse_rtt(&out);

    // 计数器增量
    let mut errn = 0i64;
    let (mut d_err, mut d_drop, mut d_crc, mut d_col) = (0i64, 0i64, 0i64, 0i64);
    if DO_NIC_STATS {
        d_err = (nic_stat(nic, "rx_errors") - re0) + (nic_stat(nic, "tx_errors") - te0);
        d_drop = (nic_stat(nic, "rx_dropped") - rd0) + (nic_stat(nic, "tx_dropped") - td0);
        d_crc = nic_stat(nic, "rx_crc_errors") - crc0;
        d_col = nic_stat(nic, "collisions") - col0;
        d_err = d_err.max(0);
        d_drop = d_drop.max(0);
        d_crc = d_crc.max(0);
        d_col = d_col.max(0);
        errn = d_err + d_drop + d_crc + d_col;
    }

    // 判定
    let (loss_disp, verdict) = match loss {
        None => ("—".to_string(), format!("{}NO REPLY{}", th.ylw, th.r)),
        Some(l) if l == 0.0 => (
            "0".to_string(),
            if errn > 0 {
                format!("{}WARN{}", th.ylw, th.r)
            } else {
                format!("{}PASS{}", th.grn, th.r)
            },
        ),
        Some(l) if l >= 100.0 => (fmt_num(l), format!("{}FAIL{}", th.red, th.r)),
        Some(l) => (fmt_num(l), format!("{}WARN{}", th.ylw, th.r)),
    };

    let (min_s, avg_s, max_s, jit_s) = match rtt {
        Some((mi, a, ma, j)) => (fmt_num(mi), fmt_num(a), fmt_num(ma), fmt_num(j)),
        None => ("—".into(), "—".into(), "—".into(), "—".into()),
    };

    println!(
        "  {}丢包{} {:<7} {}延迟 min/avg/max{} {}/{}/{} ms   {}抖动{} {} ms   {}",
        th.d,
        th.r,
        format!("{}%", loss_disp),
        th.d,
        th.r,
        min_s,
        avg_s,
        max_s,
        th.d,
        th.r,
        jit_s,
        verdict
    );
    if DO_NIC_STATS {
        let cclr = if errn > 0 { th.red } else { th.grn };
        println!(
            "  {}↳ 网卡计数{}  {}错误 {}  丢弃 {}  CRC {}  冲突 {}{}",
            th.d, th.r, cclr, d_err, d_drop, d_crc, d_col, th.r
        );
    }

    rows.push(Row {
        label: label.to_string(),
        loss: format!("{}%", loss_disp),
        avg: format!("{} ms", avg_s),
        jit: format!("{} ms", jit_s),
        errn,
        verdict,
    });
}

// ───────────── 汇总表 ─────────────
fn summary(rows: &[Row], nic: &str, target: &str, prechk: &str, mtu: &str) {
    let th = t();
    title("测试汇总");
    println!(
        "  {}{} {} {} {} {} {}{}",
        th.b,
        padr("档位", 7),
        padr("丢包", 8),
        padr("平均ms", 9),
        padr("抖动ms", 9),
        padr("错误", 6),
        "判定",
        th.r
    );
    rule("─");
    for r in rows {
        let ecol = if r.errn > 0 { th.red } else { "" };
        let ecol_r = if r.errn > 0 { th.r } else { "" };
        let errshow = if r.errn < 0 { "—".to_string() } else { r.errn.to_string() };
        println!(
            "  {} {} {} {} {}{}{} {}",
            padr(&r.label, 7),
            padr(&r.loss, 8),
            padr(&r.avg, 9),
            padr(&r.jit, 9),
            ecol,
            padr(&errshow, 6),
            ecol_r,
            r.verdict
        );
    }
    rule("─");
    println!(
        "  {}网卡 {}{}{}{}   {}目标 {}{}{}{}   {}采样 {}{}{} 包/档{}",
        th.d, th.r, th.b, nic, th.r, th.d, th.r, th.b, target, th.r, th.d, th.r, th.b, PING_COUNT, th.r
    );
    if !prechk.is_empty() {
        println!("  {}预检 {}{}", th.d, th.r, prechk);
    }
    if !mtu.is_empty() {
        println!("  {}MTU  {}{}{}{}", th.d, th.r, th.b, mtu, th.r);
    }
}

// ───────────── 图表: 各档平均延迟折线图 ─────────────
fn chart(rows: &[Row]) {
    let th = t();
    if !DO_CHART {
        return;
    }
    let mut labels: Vec<String> = vec![];
    let mut vals: Vec<f64> = vec![];
    for r in rows {
        if let Some(tok) = r.avg.split_whitespace().next() {
            if let Ok(v) = tok.parse::<f64>() {
                labels.push(r.label.clone());
                vals.push(v);
            }
        }
    }
    let n = vals.len();
    if n == 0 {
        return;
    }

    title("延迟折线图");
    note(&format!(
        "纵轴=平均延迟(ms)  横轴=档位  {}●{}{}=采样点 (自动缩放, 顶=最大 底=最小)",
        th.grn, th.r, th.gry
    ));

    let ymax = vals.iter().cloned().fold(f64::MIN, f64::max);
    let ymin = vals.iter().cloned().fold(f64::MAX, f64::min);
    let mut rng = ymax - ymin;
    if rng <= 0.0 {
        rng = if ymax > 0.0 { ymax } else { 1.0 };
    }

    const PH: i32 = 10;
    const STEP: i32 = 8;
    let mut xs: Vec<i32> = vec![];
    let mut yr: Vec<i32> = vec![];
    for i in 0..n {
        xs.push(i as i32 * STEP);
        let mut rf = (ymax - vals[i]) / rng * ((PH - 1) as f64);
        if rf < 0.0 {
            rf = 0.0;
        }
        if rf > (PH - 1) as f64 {
            rf = (PH - 1) as f64;
        }
        yr.push((rf + 0.5) as i32);
    }
    let maxc = (n as i32 - 1) * STEP;

    let mut g: HashMap<(i32, i32), char> = HashMap::new();
    for i in 0..n {
        g.insert((yr[i], xs[i]), '●');
    }
    for seg in 0..n.saturating_sub(1) {
        let (x0, y0, x1, y1) = (xs[seg], yr[seg], xs[seg + 1], yr[seg + 1]);
        let lc = if y1 < y0 {
            '/'
        } else if y1 > y0 {
            '\\'
        } else {
            '-'
        };
        let mut prev = y0;
        for cc in (x0 + 1)..x1 {
            let yv = y0 as f64 + (y1 - y0) as f64 * (cc - x0) as f64 / (x1 - x0) as f64;
            let yri = (yv + 0.5) as i32;
            let (mut lo, mut hi) = (prev, yri);
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            for k in lo..=hi {
                g.entry((k, cc)).or_insert(lc);
            }
            prev = yri;
        }
    }

    for rr in 0..PH {
        let yval = ymax - rr as f64 * rng / ((PH - 1) as f64);
        let mut line = String::new();
        for ccc in 0..=maxc {
            match g.get(&(rr, ccc)) {
                Some('●') => line.push_str(&format!("{}●{}", th.grn, th.r)),
                Some(c) => line.push_str(&format!("{}{}{}", th.cyn, c, th.r)),
                None => line.push(' '),
            }
        }
        println!("  {}{:>6}{} {}│{}{}", th.d, format!("{:.2}", yval), th.r, th.gry, th.r, line);
    }
    // x 轴
    print!("  {:>6} {}└", "", th.gry);
    for _ in 0..=maxc {
        print!("─");
    }
    println!("{}", th.r);
    // 档位标签
    let mut lblline = String::new();
    let mut pos = 0i32;
    for i in 0..n {
        while pos < xs[i] {
            lblline.push(' ');
            pos += 1;
        }
        lblline.push_str(&labels[i]);
        pos += labels[i].chars().count() as i32;
    }
    println!("  {:>6}  {}{}{}", "", th.b, lblline, th.r);
}

// ───────────── 退出清理: 恢复网卡自动协商 ─────────────
fn restore_nic(nic: &str) {
    if nic.is_empty() {
        return;
    }
    let _ = run_sudo(&["ethtool", "-s", nic, "autoneg", "on"]);
}

// 单键继续 (raw 模式经 stty; 非终端则退化为回车)
fn press_any_key() {
    if !io::stdin().is_terminal() {
        let _ = read_line();
        return;
    }
    let _ = Command::new("stty")
        .args(["-echo", "-icanon", "min", "1"])
        .stdin(Stdio::inherit())
        .status();
    let mut buf = [0u8; 1];
    let _ = io::stdin().read(&mut buf);
    let _ = Command::new("stty")
        .args(["echo", "icanon"])
        .stdin(Stdio::inherit())
        .status();
    println!();
}

fn main() {
    let color = io::stdout().is_terminal();
    let _ = THEME.set(Theme::new(color));

    // Ctrl+C / TERM: 恢复网卡自动协商后退出
    let _ = ctrlc::set_handler(|| {
        let nic = CURRENT_NIC.lock().unwrap().clone();
        println!();
        if let Some(n) = nic {
            if !n.is_empty() {
                restore_nic(&n);
                let th = t();
                println!("  {}已恢复 {} 自动协商, 再见{}", th.gry, n, th.r);
            }
        }
        std::process::exit(0);
    });

    let th = t();
    let mut last_ip = load_conf();
    boot();
    let mut show_all = false;

    loop {
        banner();
        cache_sudo();
        preflight();

        let nic = choose_nic(&mut show_all);
        *CURRENT_NIC.lock().unwrap() = Some(nic.clone());

        let target = choose_target(&nic, &mut last_ip);

        let (proceed, prechk) = connectivity_check(&target);
        if !proceed {
            continue;
        }
        let mtu = mtu_probe(&nic, &target);

        let mut rows: Vec<Row> = vec![];
        run_test("100M", &nic, &target, &["autoneg", "off", "speed", "100", "duplex", "full"], &mut rows);
        thread::sleep(Duration::from_secs(1));
        run_test("10M", &nic, &target, &["autoneg", "off", "speed", "10", "duplex", "full"], &mut rows);
        thread::sleep(Duration::from_secs(1));
        run_test("1000M", &nic, &target, &["autoneg", "on"], &mut rows);

        summary(&rows, &nic, &target, &prechk, &mtu);
        chart(&rows);
        println!();
        print!("  {}Ctrl+C 退出 · 任意键继续下一轮{} ", th.d, th.r);
        flush();
        press_any_key();
    }
}
