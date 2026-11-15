use super::*;

fn up(names: &[&str], default_route: bool) -> Sample {
    Sample {
        interfaces: names.iter().map(|s| (*s).to_owned()).collect(),
        default_route,
    }
}

fn gone() -> Sample {
    up(&[], false)
}

/// Feeds `readings` one [`SAMPLE`] apart from `t0`; returns each verdict.
fn run(rule: &mut Rule, t0: Instant, readings: &[Option<Sample>], held: bool) -> Vec<Verdict> {
    readings
        .iter()
        .enumerate()
        .map(|(i, r)| rule.observe(r.as_ref(), t0 + SAMPLE * i as u32, held))
        .collect()
}

#[test]
fn lost_for_the_whole_hold_exits() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    assert_eq!(
        rule.observe(Some(&up(&["eth0", "wg0"], true)), t0, false),
        Verdict::Fine
    );
    // gluetun's container restarts: only lo is left.
    let t1 = t0 + SAMPLE;
    assert_eq!(
        rule.observe(Some(&gone()), t1, false),
        Verdict::Losing(Duration::ZERO)
    );
    assert_eq!(
        rule.observe(Some(&gone()), t1 + HOLD - Duration::from_secs(1), false),
        Verdict::Losing(HOLD - Duration::from_secs(1))
    );
    assert_eq!(rule.observe(Some(&gone()), t1 + HOLD, false), Verdict::Exit);
}

#[test]
fn twenty_nine_seconds_is_not_enough() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    rule.observe(Some(&up(&["eth0"], true)), t0, false);
    let t1 = t0 + SAMPLE;
    rule.observe(Some(&gone()), t1, false);
    assert!(matches!(
        rule.observe(Some(&gone()), t1 + Duration::from_secs(29), false),
        Verdict::Losing(_)
    ));
}

#[test]
fn the_hold_restarts_whenever_the_network_comes_back() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    // Up, then 25 s lost, then back for one reading, then lost again.
    let mut readings = vec![Some(up(&["eth0"], true))];
    readings.extend((0..6).map(|_| Some(gone())));
    readings.push(Some(up(&["eth0"], true)));
    readings.extend((0..6).map(|_| Some(gone())));
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(!verdicts.contains(&Verdict::Exit), "{verdicts:?}");
    assert_eq!(verdicts[7], Verdict::Fine);
    // One more lost reading completes the second hold.
    let next = t0 + SAMPLE * readings.len() as u32;
    assert_eq!(rule.observe(Some(&gone()), next, false), Verdict::Exit);
}

/// gluetun's own VPN restart takes wg0 down (or away) but leaves eth0 and
/// its default route: never a loss, however long it lasts.
#[test]
fn a_vpn_restart_inside_gluetun_never_trips_it() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let mut readings = vec![Some(up(&["eth0", "wg0"], true))];
    readings.extend((0..60).map(|_| Some(up(&["eth0"], true))));
    // eth0 still listed but its default route gone for a while (gluetun
    // rewrites its routes): still not "nothing but lo".
    readings.extend((0..60).map(|_| Some(up(&["eth0"], false))));
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(verdicts.iter().all(|v| *v == Verdict::Fine), "{verdicts:?}");
}

/// Only lo plus a default route (which can't really happen, but) isn't
/// "lost": both halves of the rule must hold.
#[test]
fn a_default_route_alone_keeps_it_fine() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let mut readings = vec![Some(up(&["eth0"], true))];
    readings.extend((0..20).map(|_| Some(up(&[], true))));
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(verdicts.iter().all(|v| *v == Verdict::Fine), "{verdicts:?}");
}

/// A namespace that never had another interface (`--network=none`, or an
/// offline laptop with only lo) never arms, so it never exits.
#[test]
fn never_armed_never_exits() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let readings: Vec<_> = (0..100).map(|_| Some(gone())).collect();
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(
        verdicts.iter().all(|v| *v == Verdict::Unarmed),
        "{verdicts:?}"
    );
}

/// A laptop whose Wi-Fi goes away keeps its interface listed (down): no
/// exit, even with no default route. (It's also never enabled there.)
#[test]
fn an_interface_that_is_down_still_counts() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let readings: Vec<_> = (0..100).map(|_| Some(up(&["wlan0"], false))).collect();
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(verdicts.iter().all(|v| *v == Verdict::Fine), "{verdicts:?}");
}

#[test]
fn a_failed_read_never_exits_and_breaks_the_hold() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let mut readings = vec![Some(up(&["eth0"], true))];
    for _ in 0..20 {
        readings.extend((0..5).map(|_| Some(gone())));
        readings.push(None);
    }
    let verdicts = run(&mut rule, t0, &readings, false);
    assert!(!verdicts.contains(&Verdict::Exit), "{verdicts:?}");
    // Unreadable before it was ever armed: still unarmed.
    let mut fresh = Rule::default();
    assert_eq!(fresh.observe(None, t0, false), Verdict::Unarmed);
}

#[test]
fn held_during_an_update_self_test() {
    let mut rule = Rule::default();
    let t0 = Instant::now();
    let mut readings = vec![Some(up(&["eth0"], true))];
    readings.extend((0..20).map(|_| Some(gone())));
    let verdicts = run(&mut rule, t0, &readings, true);
    assert_eq!(verdicts[0], Verdict::Held);
    assert!(verdicts.iter().all(|v| *v == Verdict::Held), "{verdicts:?}");
    // Once the self-test has decided, the hold starts from scratch.
    let after = t0 + SAMPLE * readings.len() as u32;
    assert_eq!(
        rule.observe(Some(&gone()), after, false),
        Verdict::Losing(Duration::ZERO)
    );
}

#[test]
fn parses_proc_net_dev() {
    let text = "Inter-|   Receive                                                |  Transmit\n \
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
lo:  123  4 0 0 0 0 0 0  123 4 0 0 0 0 0 0\n  \
eth0: 9 1 0 0 0 0 0 0 9 1 0 0 0 0 0 0\n   \
wg0:1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\n";
    assert_eq!(parse_dev(text), vec!["eth0".to_owned(), "wg0".to_owned()]);
    let only_lo = "h1\nh2\n    lo: 1 2 3\n";
    assert!(parse_dev(only_lo).is_empty());
    assert!(parse_dev("").is_empty());
}

#[test]
fn parses_the_ipv4_route_table() {
    let head =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
    let default = "eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
    let subnet = "eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
    assert!(parse_route_v4(&format!("{head}{subnet}{default}")));
    assert!(!parse_route_v4(&format!("{head}{subnet}")));
    assert!(!parse_route_v4(head));
    // Down, rejected or through lo: not a way out.
    assert!(!parse_route_v4(&format!(
        "{head}eth0\t00000000\t010011AC\t0002\t0\t0\t0\t00000000\t0\t0\t0\n"
    )));
    assert!(!parse_route_v4(&format!(
        "{head}eth0\t00000000\t00000000\t0201\t0\t0\t0\t00000000\t0\t0\t0\n"
    )));
    assert!(!parse_route_v4(&format!(
        "{head}lo\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0\n"
    )));
    // A truncated line is skipped.
    assert!(!parse_route_v4(&format!("{head}eth0\t00000000\n")));
}

#[test]
fn parses_the_ipv6_route_table() {
    let z = "00000000000000000000000000000000";
    let unreachable = format!("{z} 00 {z} 00 {z} ffffffff 00000001 00000000 00200200       lo\n");
    let link = format!(
        "fe800000000000000000000000000000 40 {z} 00 {z} 00000100 00000001 00000000 00000001     eth0\n"
    );
    let default = format!(
        "{z} 00 {z} 00 fe800000000000000000000000000001 00000400 00000001 00000000 00450003     eth0\n"
    );
    assert!(!parse_route_v6(&unreachable), "the kernel's lo entry");
    assert!(!parse_route_v6(&format!("{unreachable}{link}")));
    assert!(parse_route_v6(&format!("{unreachable}{link}{default}")));
    let rejected = format!("{z} 00 {z} 00 {z} 00000400 00000001 00000000 00000201     eth0\n");
    assert!(!parse_route_v6(&rejected));
    assert!(!parse_route_v6(""));
}

#[test]
fn builds_a_sample_and_never_guesses() {
    let dev = "h\nh\n lo: 1\n";
    assert_eq!(sample_from(Some(dev), None, None), Some(gone()));
    assert_eq!(
        sample_from(None, Some(""), Some("")),
        None,
        "no interface list"
    );
    let head = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n";
    let v4 = format!("{head}eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\n");
    let s = sample_from(Some("h\nh\n eth0: 1\n"), Some(&v4), None).expect("sample");
    assert_eq!(s, up(&["eth0"], true));
    assert!(!s.lost());
    assert!(gone().lost());
}

#[test]
fn the_exit_code_is_a_temporary_failure() {
    assert_eq!(EXIT_CODE, 75);
    assert_eq!(SAMPLE, Duration::from_secs(5));
    assert_eq!(HOLD, Duration::from_secs(30));
}

/// Whether `lost(enabled)` is still pending after its first poll.
async fn pending_after_first_poll(enabled: bool) -> bool {
    tokio::select! {
        biased;
        () = lost(enabled) => false,
        () = std::future::ready(()) => true,
    }
}

/// Off, it never resolves (on any platform): it doesn't even read.
#[tokio::test]
async fn off_never_resolves() {
    assert!(pending_after_first_poll(false).await);
}

/// Linux reads this process's own namespace; the test runner has a real
/// network or at least lo, so a reading exists.
#[cfg(target_os = "linux")]
#[test]
fn reads_this_namespace_on_linux() {
    assert!(read().is_some(), "/proc/self/net/dev is readable");
}

/// Elsewhere the watch isn't supported: enabled, it logs once and never
/// resolves, so a native install never exits from it.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn elsewhere_it_is_ignored() {
    assert!(pending_after_first_poll(true).await);
}
