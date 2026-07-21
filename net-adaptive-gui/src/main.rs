// 网卡自适应测速 · Slint (Material) 图形界面
// Windows 上无 GUI 时也能编译验证; 实际运行在带图形桌面的 ARM Linux 上。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod core;

use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;

slint::include_modules!();

const SETTLE: u32 = core::SETTLE_SEC;

// 判定颜色
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

// 传给 UI 线程的档位快照 (Send)
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
    avg_val: Option<f64>,
}

fn nic_detail(n: &core::Nic) -> String {
    let ip = n.ipv4.clone().unwrap_or_else(|| "无 IPv4".into());
    let kind = if n.physical { "物理" } else { "虚拟" };
    format!("{} · {} · MTU {} · {}", n.state, ip, n.mtu, kind)
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    // 网卡状态 (UI 线程持有)
    let nics: Rc<std::cell::RefCell<Vec<core::Nic>>> = Rc::new(std::cell::RefCell::new(vec![]));
    // 正在测试的网卡 (退出时恢复自动协商)
    let active_nic: Rc<std::cell::RefCell<Option<String>>> = Rc::new(std::cell::RefCell::new(None));

    // ── 刷新网卡列表 ──
    let refresh = {
        let ui = ui.as_weak();
        let nics = nics.clone();
        move || {
            let list = core::list_nics(false);
            let names: Vec<SharedString> =
                list.iter().map(|n| SharedString::from(n.name.as_str())).collect();
            let detail = list.first().map(nic_detail).unwrap_or_else(|| "未检测到网卡".into());
            *nics.borrow_mut() = list;
            if let Some(ui) = ui.upgrade() {
                ui.set_nic_names(ModelRc::from(Rc::new(VecModel::from(names))));
                if ui.get_selected_nic() < 0 {
                    ui.set_selected_nic(0);
                }
                ui.set_nic_detail(detail.into());
            }
        }
    };
    refresh(); // 启动即加载
    {
        let refresh = refresh.clone();
        ui.on_refresh_nics(move || refresh());
    }

    // ── 切换网卡 -> 更新详情 ──
    {
        let ui = ui.as_weak();
        let nics = nics.clone();
        ui.clone().upgrade().unwrap().on_nic_changed(move |idx| {
            if let Some(ui) = ui.upgrade() {
                let list = nics.borrow();
                if let Some(n) = list.get(idx.max(0) as usize) {
                    ui.set_nic_detail(nic_detail(n).into());
                }
            }
        });
    }

    // ── 打开扫描浮层 & 开始扫描 ──
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
            ui.set_has_chart(false);
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

// 后台测速流程
fn run_test_worker(
    w: slint::Weak<AppWindow>,
    nic: String,
    target: String,
    do_mtu: bool,
    do_stats: bool,
    count: u32,
) {
    core::cache_sudo();

    // 1) 连通性预检
    set_status(&w, "连通性预检…".into());
    let chk = core::connectivity(&target, 5);
    {
        let (text, rgb) = if chk.reachable {
            (
                format!(
                    "可达 · 丢包 {}% · {} ms",
                    core::fmt_opt(chk.loss),
                    core::fmt_opt(chk.avg)
                ),
                (0x0c, 0xa3, 0x0c),
            )
        } else {
            ("不可达 (100% 丢包 / 无响应)".to_string(), (0xd0, 0x3b, 0x3b))
        };
        let _ = w.upgrade_in_event_loop(move |ui| {
            ui.set_precheck_text(text.into());
            ui.set_precheck_color(col(rgb));
        });
    }

    // 2) MTU 探测
    if do_mtu {
        set_status(&w, "MTU 大包探测…".into());
        let m = core::mtu_probe(&nic, &target);
        let text = if m.skipped {
            "跳过 (DF 小包不通)".to_string()
        } else if m.full {
            format!("{} (直通)", m.path_mtu)
        } else {
            format!("{} (< 网卡 {})", m.path_mtu, m.link_mtu)
        };
        let _ = w.upgrade_in_event_loop(move |ui| ui.set_mtu_text(text.into()));
    }

    // 3) 逐档测试
    let tiers: [(&str, Vec<&str>); 3] = [
        ("100M", vec!["autoneg", "off", "speed", "100", "duplex", "full"]),
        ("10M", vec!["autoneg", "off", "speed", "10", "duplex", "full"]),
        ("1000M", vec!["autoneg", "on"]),
    ];

    let mut acc: Vec<TierData> = vec![];
    for (label, args) in tiers.iter() {
        set_status(&w, format!("测试 {}: 设置协商…", label));
        if !core::set_tier(&nic, args) {
            acc.push(TierData {
                label: label.to_string(),
                loss: "—".into(),
                avg: "—".into(),
                jit: "—".into(),
                err: "—".into(),
                verdict: "ERROR".into(),
                rgb: (0xd0, 0x3b, 0x3b),
                link: "设置失败".into(),
                avg_val: None,
            });
            push_tiers(&w, &acc);
            continue;
        }
        // 协商稳定倒计时
        for s in (1..=SETTLE).rev() {
            set_status(&w, format!("{} 链路协商稳定中… {}s", label, s));
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        set_status(&w, format!("{} 发包测试 ({} 包)…", label, count));
        let t = core::run_ping_tier(&nic, &target, count, do_stats);

        let loss = match t.loss {
            Some(l) => format!("{}%", core::fmt_num(l)),
            None => "—".into(),
        };
        acc.push(TierData {
            label: label.to_string(),
            loss,
            avg: core::fmt_opt(t.avg),
            jit: core::fmt_opt(t.jit),
            err: t.errn.to_string(),
            verdict: verdict_text(t.verdict).to_string(),
            rgb: verdict_rgb(t.verdict),
            link: format!("{} · {}", t.speed, t.duplex),
            avg_val: t.avg,
        });
        push_tiers(&w, &acc);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    set_status(&w, "完成".into());
    let _ = w.upgrade_in_event_loop(|ui| ui.set_busy(false));
}

// 把累计档位推给 UI, 并重算折线图
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
        build_chart(&ui, &snapshot);
    });
}

// 折线图: 分数坐标 (0..1), Path 用 0..100 视图框
fn build_chart(ui: &AppWindow, data: &[TierData]) {
    let pts: Vec<(&str, f64)> = data
        .iter()
        .filter_map(|d| d.avg_val.map(|v| (d.label.as_str(), v)))
        .collect();
    let n = pts.len();
    if n == 0 {
        ui.set_has_chart(false);
        ui.set_chart_points(ModelRc::from(Rc::new(VecModel::from(Vec::<ChartPoint>::new()))));
        ui.set_chart_path("".into());
        return;
    }
    let ymax = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let ymin = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let rng = ymax - ymin;

    let xf = |i: usize| -> f64 {
        if n == 1 {
            0.5
        } else {
            0.10 + 0.80 * (i as f64 / (n - 1) as f64)
        }
    };
    let yf = |v: f64| -> f64 {
        if rng.abs() < 1e-9 {
            0.5
        } else {
            0.14 + (0.86 - 0.14) * ((ymax - v) / rng)
        }
    };

    let mut cmd = String::new();
    let mut points: Vec<ChartPoint> = vec![];
    for (i, (label, v)) in pts.iter().enumerate() {
        let x = xf(i);
        let y = yf(*v);
        if i == 0 {
            cmd.push_str(&format!("M {:.2} {:.2}", x * 100.0, y * 100.0));
        } else {
            cmd.push_str(&format!(" L {:.2} {:.2}", x * 100.0, y * 100.0));
        }
        points.push(ChartPoint {
            x: x as f32,
            y: y as f32,
            value: core::fmt_num(*v).into(),
            label: (*label).into(),
        });
    }

    ui.set_chart_path(cmd.into());
    ui.set_chart_points(ModelRc::from(Rc::new(VecModel::from(points))));
    ui.set_chart_ymax(format!("{} ms", core::fmt_num(ymax)).into());
    ui.set_chart_ymin(format!("{} ms", core::fmt_num(ymin)).into());
    ui.set_has_chart(true);
}
