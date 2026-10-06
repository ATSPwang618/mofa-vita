use pvr_compiler::{Compiler, Stage};
use std::{
    env, fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

const WORKER_ARG: &str = "--compile-sgx-worker";

pub fn run_worker(sources: &[(Stage, String)], output: &Path) -> bool {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some(WORKER_ARG) {
        return false;
    }
    let offset = args.next().unwrap().parse::<usize>().unwrap();
    let workers = args.next().unwrap().parse::<usize>().unwrap();
    assert!(offset < workers && args.next().is_none());
    compile_shard(sources, output, offset, workers);
    true
}

pub fn compile(sources: &[(Stage, String)], output: &Path) {
    let started = Instant::now();
    let limit = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(
            env::var("NUM_JOBS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
        )
        .min(sources.len())
        .max(1);
    // SAFETY: Cargo supplies the jobserver handles to its build script. This
    // runs once, before spawning workers or opening any compiler context.
    let jobserver = unsafe { jobserver::Client::from_env() };
    // The build script already owns one implicit Cargo slot. Extra processes
    // need tokens; never block acquiring them while holding that implicit slot.
    let mut permits = vec![None];
    if let Some(client) = jobserver {
        while permits.len() < limit {
            match client.try_acquire().expect("acquire SGX compiler slot") {
                Some(permit) => permits.push(Some(permit)),
                None => break,
            }
        }
    }
    let workers = permits.len();
    println!(
        "cargo:warning=Precompiling {} SGX543 shaders with {workers} compiler processes",
        sources.len()
    );
    if workers == 1 {
        compile_shard(sources, output, 0, 1);
    } else {
        let executable = env::current_exe().expect("locate SGX build script");
        let mut children = Vec::with_capacity(workers);
        for (offset, permit) in permits.into_iter().enumerate() {
            let child = Command::new(&executable)
                .arg(WORKER_ARG)
                .arg(offset.to_string())
                .arg(workers.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .expect("start SGX compiler worker");
            children.push(Worker {
                child,
                permit,
                finished: false,
            });
        }
        for (offset, worker) in children.iter_mut().enumerate() {
            let status = worker.child.wait().expect("wait for SGX compiler worker");
            worker.finished = true;
            worker.permit.take();
            assert!(status.success(), "SGX compiler worker {offset}: {status}");
        }
    }
    println!(
        "cargo:warning=SGX543 shader compilation finished in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}

fn compile_shard(sources: &[(Stage, String)], output: &Path, offset: usize, workers: usize) {
    // Native parser globals are protected by a process-wide mutex. Each child
    // must own its context; threads within one process would still serialize.
    let mut compiler =
        Compiler::new().expect("SGX binary generation requires a Linux/WSL build host");
    for index in (offset..sources.len()).step_by(workers) {
        let (stage, source) = &sources[index];
        let compiled = compiler
            .compile_binary(*stage, source)
            .unwrap_or_else(|error| panic!("serialize SGX shader {index}: {error}"));
        assert!(
            compiled.success && !compiled.binary.is_empty(),
            "SGX shader {index}: {}",
            compiled.log
        );
        fs::write(output.join(format!("sgx-{index}.glsl")), source).unwrap();
        fs::write(output.join(format!("sgx-{index}.bin")), &compiled.binary).unwrap();
    }
}

struct Worker {
    child: Child,
    permit: Option<jobserver::Acquired>,
    finished: bool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A spawn, wait or compilation failure must not leave other workers
        // writing into OUT_DIR after Cargo has reported a failed build.
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
