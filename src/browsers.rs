use std::process::Command;

use crate::human;

pub struct Browser {
    pid: u32,
    // tool is bro or peek, read from the profile folder each one names after itself.
    tool: String,
    port: String,
    headless: bool,
    started: String,
    // memory_kb adds the main process and its helpers.
    memory_kb: u64,
}

// find returns every Chrome started with --remote-debugging-port, which is how
// bro and peek launch theirs; the user's own Chrome never carries that flag.
pub fn find() -> Vec<Browser> {
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,ppid=,rss=,lstart=,command="]).output() else {
        return Vec::new();
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    let processes: Vec<(u32, u32, u64, String, &str)> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let rss_kb: u64 = fields.next()?.parse().ok()?;
            let started = fields.take(5).collect::<Vec<_>>().join(" ");
            let command_start = line.find(" /")?;
            Some((pid, ppid, rss_kb, started, line[command_start..].trim()))
        })
        .collect();

    processes
        .iter()
        .filter(|(_, _, _, _, command)| {
            let executable = command.split(" --").next().unwrap_or("");
            executable.ends_with("/Contents/MacOS/Google Chrome") && command.contains(" --remote-debugging-port")
        })
        .map(|(pid, _, rss_kb, started, command)| {
            let helpers: u64 = processes.iter().filter(|(_, ppid, ..)| ppid == pid).map(|(_, _, rss, ..)| rss).sum();
            let profile = flag(command, "--user-data-dir=").rsplit('/').next().unwrap_or("").to_string();
            Browser {
                pid: *pid,
                tool: profile.split("-chrome-").next().filter(|_| profile.contains("-chrome-")).unwrap_or("?").to_string(),
                port: flag(command, "--remote-debugging-port=").to_string(),
                headless: command.contains(" --headless"),
                started: started.clone(),
                memory_kb: rss_kb + helpers,
            }
        })
        .collect()
}

fn flag<'a>(command: &'a str, name: &str) -> &'a str {
    command.split(name).nth(1).and_then(|rest| rest.split(" --").next()).unwrap_or("")
}

pub fn list(report: impl Fn(String)) {
    let browsers = find();
    if browsers.is_empty() {
        report("No bro/peek browsers open".to_string());
        return;
    }
    for browser in &browsers {
        let mode = if browser.headless { "headless" } else { "window" };
        report(format!(
            "{:<5} port {:<5} {:<8} {:>7}  pid {:<6} since {}",
            browser.tool,
            browser.port,
            mode,
            human(browser.memory_kb * 1024),
            browser.pid,
            browser.started
        ));
    }
    let total: u64 = browsers.iter().map(|browser| browser.memory_kb).sum();
    report(format!("{} browsers, {} of memory", browsers.len(), human(total * 1024)));
}

// kill ends them all; killing the main process takes its helpers with it.
pub fn kill() -> Option<String> {
    let browsers = find();
    if browsers.is_empty() {
        return None;
    }
    let memory_kb: u64 = browsers.iter().map(|browser| browser.memory_kb).sum();
    let pids: Vec<String> = browsers.iter().map(|browser| browser.pid.to_string()).collect();
    match Command::new("kill").args(&pids).status() {
        Ok(status) if status.success() => {
            Some(format!("Killed {} bro/peek browsers ({} of memory)", browsers.len(), human(memory_kb * 1024)))
        }
        _ => Some(format!("kill {} failed", pids.join(" "))),
    }
}
