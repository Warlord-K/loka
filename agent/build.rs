use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    println!("cargo:rerun-if-env-changed=WORLD_BUILD_SHA");
    if let Some(dir) = git(&["rev-parse", "--path-format=absolute", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
            println!("cargo:rerun-if-changed={common}/refs/heads");
            println!("cargo:rerun-if-changed={common}/packed-refs");
        }
    }
    let sha = std::env::var("WORLD_BUILD_SHA").ok().filter(|s| !s.is_empty()).or_else(|| git(&["rev-parse", "--short=10", "HEAD"]));
    let date = git(&["show", "-s", "--format=%cs", "HEAD"]);
    let build = match (sha, date) {
        (Some(sha), Some(date)) => format!("{sha} ({date})"),
        (Some(sha), None) => sha,
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=WORLD_BUILD={build}");
}
