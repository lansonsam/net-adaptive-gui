// 网卡自适应测速 · Slint (Material) 图形界面
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod core;

use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

slint::include_modules!();

const SETTLE: u32 = core::SETTLE_SEC;

fn verdict_rgb(v: core::Verdict) -> (u8, u8, u8) {
    match v {
        core::Verdict::Pass => (0x0c, 0xa3, 0x0c),
        core::Verdict::Warn | core::Verdict::NoReply => (0xfa, 0xb2, 0x19),
        core::Verdict::Fail | core::Verdict::Error => (0xd0, 0x3b, 0x3b),
    }
}
fn verdict_text(v: core::Verdict) -> &'static str {
    match v {
        core::Verdict::Pass => "PASS",
        core::Verdict::Warn => "WARN",
        core::Verdict::Fail => "FAIL",
        core::Verdict::NoReply => "NO REPLY",
        core::Verdict::Error => "ERROR",
    }
}
fn col(rgb: (u8, u8, u8)) -> Color {
    Color::from_rgb_u8(rgb.0, rgb.1, rgb.2)
}

#[derive(Clone)]
struct TierData {
    label: String,
    loss: String,
    avg: String,
    jit: String,
    err: String,
    verdict: String,
    rgb: (u8, u8, u8),
    link: String,
}

fn nic_detail(n: &core::Nic) -> String {
    let ip = n.ipv4.clone().unwrap_or_else(|| "无 IPv4".into());
    let kind = if n.physical { "物理" } else { "虚拟" };
    format!("{} · {} · MTU {} · {}", n.state, ip, n.mtu, kind)
}
fn nic_row(n: &core::Nic) -> NicRow {
    let ip = n.ipv4.clone().unwrap_or_else(|| "无 IPv4".into());
    NicRow {
        name: n.name.as_str().into(),
        detail: format!("{} · {} · MTU {}", n.state, ip, n.mtu).into(),
        up: n.state == "up",
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    let nics: Rc<std::cell::RefCell<Vec<core::Nic>>> = Rc::new(std::cell::RefCell::new(vec![]));
    let active_nic: Rc<std::cell::RefCell<Option<String>>> = Rc::new(std::cell::RefCell::new(None));

    // ── 刷新网卡列表 ──
    let refresh = {
        let ui = ui.as_weak();
        let nics = nics.clone();
        move || {
            let list = core::list_nics(false);
            let rows: Vec<NicRow> = list.iter().map(nic_row).collect();
            if let Some(ui) = ui.upgrade() {
                ui.set_nic_list(ModelRc::from(Rc::new(VecModel::from(rows))));
                let mut sel = ui.get_selected_nic();
                if sel < 0 || sel as usize >= list.len() {
                    sel = 0;
                    ui.set_selected_nic(0);
                }
                if let Some(n) = list.get(sel as usize) {
                    ui.set_selected_nic_name(n.name.as_str().into());
                    ui.set_nic_detail(nic_detail(n).into());
                } else {
                    ui.set_selected_nic_name("".into());
                    ui.set_nic_detail("未检测到网卡".into());
                }
            }
            *nics.borrow_mut() = list;
        }
    };
    refresh();
    {
        let refresh = refresh.clone();
        ui.on_refresh_nics(move || refresh());
    }

    // ── 网卡弹窗 ──
    {
        let refresh = refresh.clone();
        let ui = ui.as_weak();
        ui.clone().upgrade().unwrap().on_open_nics(move || {
            refresh();
            if let Some(ui) = ui.upgrade() {
                ui.set_show_nics(true);
            }
        });
    }
    ui.on_close_nics({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_show_nics(false);
            }
        }
    });
    ui.on_pick_nic({
        let ui = ui.as_weak();
        let nics = nics.clone();
        move |i| {
            if let Some(ui) = ui.upgrade() {
                if let Some(n) = nics.borrow().get(i.max(0) as usize) {
                    ui.set_selected_nic(i);
                    ui.set_selected_nic_name(n.name.as_str().into());
                    ui.set_nic_detail(nic_detail(n).into());
                }
                ui.set_show_nics(false);
            }
        }
    });

    // ── 局域网扫描 ──
    {
        let ui_weak = ui.as_weak();
        let nics = nics.clone();
        ui.on_open_scan(move || {
            let nic = {
                let list = nics.borrow();
                let idx = ui_weak.upgrade().map(|u| u.get_selected_nic()).unwrap_or(0).max(0) as usize;
                match list.get(idx) {
                    Some(n) => n.name.clone(),
                    None => return,
                }
            };
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_show_hosts(true);
                ui.set_scanning(true);
                ui.set_scan_progress(0.0);
                ui.set_hosts(ModelRc::from(Rc::new(VecModel::from(Vec::<HostRow>::new()))));
            }
            let ui_weak2 = ui_weak.clone();
            std::thread::spawn(move || {
                let prog = ui_weak2.clone();
                let result = core::scan_hosts(&nic, move |done, total| {
                    let f = done as f32 / total.max(1) as f32;
                    let _ = prog.upgrade_in_event_loop(move |ui| ui.set_scan_progress(f));
                });
                let _ = ui_weak2.upgrade_in_event_loop(move |ui| {
                    ui.set_scanning(false);
                    match result {
                        Ok(hosts) => {
                            let rows: Vec<HostRow> = hosts
                                .into_iter()
                                .map(|h| HostRow {
                                    ip: h.ip.into(),
                                    mac: h.mac.into(),
                                    is_self: h.is_self,
                                })
                                .collect();
                            ui.set_hosts(ModelRc::from(Rc::new(VecModel::from(rows))));
                        }
                        Err(e) => ui.set_status(e.into()),
                    }
                });
            });
        });
    }
    ui.on_close_scan({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_show_hosts(false);
            }
        }
    });
    ui.on_pick_host({
        let ui = ui.as_weak();
        move |i| {
            if let Some(ui) = ui.upgrade() {
                if let Some(h) = ui.get_hosts().row_data(i as usize) {
                    ui.set_target(h.ip);
                }
                ui.set_show_hosts(false);
            }
        }
    });

    // ── 开始测试 ──
    {
        let ui_weak = ui.as_weak();
        let nics = nics.clone();
        let active_nic = active_nic.clone();
        ui.on_start_test(move || {
            let ui = match ui_weak.upgrade() {
                Some(u) => u,
                None => return,
            };
            let idx = ui.get_selected_nic().max(0) as usize;
            let nic = match nics.borrow().get(idx) {
                Some(n) => n.name.clone(),
                None => return,
            };
            let target = ui.get_target().trim().to_string();
            if target.is_empty() {
                ui.set_status("请先填写目标 IP".into());
                return;
            }
            let do_mtu = ui.get_do_mtu();
            let do_stats = ui.get_do_stats();
            let count = ui.get_ping_count().max(1) as u32;

            *active_nic.borrow_mut() = Some(nic.clone());
            ui.set_busy(true);
            ui.set_status("准备…".into());
            ui.set_tiers(ModelRc::from(Rc::new(VecModel::from(Vec::<TierRow>::new()))));
            ui.set_log_text("".into());
            ui.set_precheck_text("检测中…".into());
            ui.set_precheck_color(Color::from_rgb_u8(0x89, 0x87, 0x81));
            ui.set_mtu_text("—".into());

            let w = ui_weak.clone();
            std::thread::spawn(move || {
                run_test_worker(w, nic, target, do_mtu, do_stats, count);
            });
        });
    }

    // ── 退出时恢复网卡自动协商 ──
    {
        let active_nic = active_nic.clone();
        ui.window().on_close_requested(move || {
            if let Some(nic) = active_nic.borrow().clone() {
                core::restore_nic(&nic);
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    ui.run()
}

fn set_status(w: &slint::Weak<AppWindow>, s: String) {
    let _ = w.upgrade_in_event_loop(move |ui| ui.set_status(s.into()));
}

// 追加一行日志并推给 UI (自动吸附到底部)
fn push_log(w: &slint::Weak<AppWindow>, log: &Arc<Mutex<String>>, line: &str) {
    let mut g = log.lock().unwrap();
    g.push_str(line);
    g.push('\n');
    let snap = g.clone();
    drop(g);
    let _ = w.upgrade_in_event_loop(move |ui| ui.set_log_text(snap.into()));
}

fn push_tiers(w: &slint::Weak<AppWindow>, acc: &[TierData]) {
    let snapshot = acc.to_vec();
    let _ = w.upgrade_in_event_loop(move |ui| {
        let rows: Vec<TierRow> = snapshot
            .iter()
            .map(|d| TierRow {
                label: d.label.as_str().into(),
                loss: d.loss.as_str().into(),
                avg: d.avg.as_str().into(),
                jit: d.jit.as_str().into(),
                err: d.err.as_str().into(),
                verdict: d.verdict.as_str().into(),
                color: col(d.rgb),
                link: d.link.as_str().into(),
            })
            .collect();
        ui.set_tiers(ModelRc::from(Rc::new(VecModel::from(rows))));
    });
}

// 后台测速流程 (全过程写入运行日志)
fn run_test_worker(
    w: slint::Weak<AppWindow>,
    nic: String,
    target: String,
    do_mtu: bool,
    do_stats: bool,
    count: u32,
) {
    let log = Arc::new(Mutex::new(String::new()));
    core::cache_sudo();

    push_log(&w, &log, &format!("[+] 目标 {} · 网卡 {}", target, nic));

    // 1) 连通性预检
    set_status(&w, "连通性预检…".into());
    push_log(&w, &log, &format!("[+] 连通性预检: ping -c 5 {}", target));
    let chk = {
        let w2 = w.clone();
        let log2 = log.clone();
        core::connectivity(&target, 5, move |l| push_log(&w2, &log2, &format!("[*] {}", l)))
    };
    let (text, rgb) = if chk.reachable {
        (
            format!("可达 · 丢包 {}% · {} ms", core::fmt_opt(chk.loss), core::fmt_opt(chk.avg)),
            (0x0c, 0xa3, 0x0c),
        )
    } else {
        ("不可达 (100% 丢包 / 无响应)".to_string(), (0xd0, 0x3b, 0x3b))
    };
    push_log(&w, &log, &format!("[+] 预检结果: {}", text));
    {
        let t = text.clone();
        let _ = w.upgrade_in_event_loop(move |ui| {
            ui.set_precheck_text(t.into());
            ui.set_precheck_color(col(rgb));
        });
    }

    // 2) MTU 探测
    if do_mtu {
        set_status(&w, "MTU 大包探测…".into());
        push_log(&w, &log, "[+] MTU 大包探测 (DF 二分)…");
        let m = core::mtu_probe(&nic, &target);
        let text = if m.skipped {
            "跳过 (DF 小包不通)".to_string()
        } else if m.full {
            format!("{} (直通)", m.path_mtu)
        } else {
            format!("{} (< 网卡 {})", m.path_mtu, m.link_mtu)
        };
        push_log(&w, &log, &format!("[+] 路径 MTU = {}", text));
        let t2 = text.clone();
        let _ = w.upgrade_in_event_loop(move |ui| ui.set_mtu_text(t2.into()));
    }

    // 3) 逐档测试
    let tiers: [(&str, Vec<&str>, &str); 3] = [
        ("100M", vec!["autoneg", "off", "speed", "100", "duplex", "full"], "autoneg off · 100Mb/s · full"),
        ("10M", vec!["autoneg", "off", "speed", "10", "duplex", "full"], "autoneg off · 10Mb/s · full"),
        ("1000M", vec!["autoneg", "on"], "autoneg on (自动协商)"),
    ];

    let mut acc: Vec<TierData> = vec![];
    for (label, args, desc) in tiers.iter() {
        push_log(&w, &log, &format!("[+] ═══ {} ═══", label));
        set_status(&w, format!("{}: 调整速率…", label));
        push_log(&w, &log, &format!("[+] 调整速率: {}", desc));
        push_log(&w, &log, &format!("[+] ethtool -s {} {}", nic, args.join(" ")));

        if !core::set_tier(&nic, args) {
            push_log(&w, &log, "[!] 网卡设置失败, 跳过本档");
            acc.push(TierData {
                label: label.to_string(),
                loss: "—".into(),
                avg: "—".into(),
                jit: "—".into(),
                err: "—".into(),
                verdict: "ERROR".into(),
                rgb: (0xd0, 0x3b, 0x3b),
                link: "设置失败".into(),
            });
            push_tiers(&w, &acc);
            continue;
        }

        // 协商稳定过程
        for s in (1..=SETTLE).rev() {
            set_status(&w, format!("{}: 链路协商稳定中 {}s", label, s));
            push_log(&w, &log, &format!("[+] 等待链路协商稳定… {}s", s));
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let (speed, duplex, link) = core::read_link(&nic);
        push_log(
            &w,
            &log,
            &format!("[+] 链路就绪: Speed={} Duplex={} Link={}", speed, duplex, if link { "yes" } else { "no" }),
        );

        // 发包 (逐包写日志)
        set_status(&w, format!("{}: 发包测试 ({} 包)…", label, count));
        push_log(&w, &log, &format!("[+] 发包: ping -c {} {}", count, target));
        let t = {
            let w2 = w.clone();
            let log2 = log.clone();
            core::run_ping_tier(&nic, &target, count, do_stats, move |l| {
                push_log(&w2, &log2, &format!("[*] {}", l))
            })
        };

        let loss = match t.loss {
            Some(l) => format!("{}%", core::fmt_num(l)),
            None => "—".into(),
        };
        let vtext = verdict_text(t.verdict);
        push_log(
            &w,
            &log,
            &format!(
                "[+] 结果: 丢包 {} · 平均 {}ms · 抖动 {}ms · 错误 {} · {}",
                loss,
                core::fmt_opt(t.avg),
                core::fmt_opt(t.jit),
                t.errn,
                vtext
            ),
        );
        acc.push(TierData {
            label: label.to_string(),
            loss,
            avg: core::fmt_opt(t.avg),
            jit: core::fmt_opt(t.jit),
            err: t.errn.to_string(),
            verdict: vtext.to_string(),
            rgb: verdict_rgb(t.verdict),
            link: format!("{} · {}", t.speed, t.duplex),
        });
        push_tiers(&w, &acc);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    push_log(&w, &log, "[+] ✓ 全部完成");
    set_status(&w, "完成".into());
    let _ = w.upgrade_in_event_loop(|ui| ui.set_busy(false));
}
