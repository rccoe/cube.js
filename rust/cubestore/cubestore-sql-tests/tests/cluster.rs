//! Runs the SQL tests with a cluster that consists of 1 router and 2 select workers.
//! Note that each worker will also spawns 2 subprocesses for actual processing.

use async_trait::async_trait;
use serde_derive::{Deserialize, Serialize};

use cubestore::config::Config;
use cubestore::util::respawn;
use cubestore_sql_tests::multiproc::{
    multiproc_child_main, run_multiproc_test, MultiProcTest, SignalInit, WaitCompletion, WorkerProc,
};
use cubestore_sql_tests::{run_sql_tests, TestFn};

const METASTORE_PORT: u16 = 51336;
const WORKER_PORTS: [u16; 2] = [51337, 51338];

#[cfg(not(target_os = "windows"))]
fn main() {
    respawn::register_handler(multiproc_child_main::<ClusterSqlTest>);
    respawn::init(); // TODO: logs in worker processes.

    // We run only 1 test in parallel to avoid using the ports concurrently.
    // We skip `planning_inplace_aggregate2` as planning results differ on cluster with 2 nodes.
    run_sql_tests(
        "cluster",
        vec![
            "--test-threads=1".to_string(),
            "--skip".to_string(),
            "planning_inplace_aggregate2".to_string(),
        ],
        |test_name, test_fn| {
            // Add a suffix to avoid clashes with other configurations run concurrently.
            // TODO: run each test in unique temp folder.
            run_multiproc_test(ClusterSqlTest {
                test_name: test_name.to_owned() + "-cluster",
                test_fn,
            });
        },
    );
}

struct ClusterSqlTest {
    test_name: String,
    test_fn: TestFn,
}

#[derive(Serialize, Deserialize)]
struct WorkerArgs {
    id: usize,
    test_name: String,
}

#[async_trait]
impl MultiProcTest for ClusterSqlTest {
    type WorkerArgs = WorkerArgs;
    type WorkerProc = WorkerFn;

    fn worker_arguments(&self) -> Vec<WorkerArgs> {
        (0..=1)
            .map(|i| WorkerArgs {
                test_name: self.test_name.clone(),
                id: i,
            })
            .collect()
    }

    async fn drive(self) {
        Config::test(&self.test_name)
            .update_config(|mut c| {
                c.server_name = format!("localhost:{}", METASTORE_PORT);
                c.metastore_bind_address = Some(c.server_name.clone());
                c.select_workers = WORKER_PORTS
                    .iter()
                    .map(|p| format!("localhost:{}", p))
                    .collect();
                c
            })
            .start_test(|services| async move {
                (self.test_fn)(Box::new(services.sql_service)).await;
            })
            .await;
    }
}

#[derive(Default)]
struct WorkerFn;
#[async_trait]
impl WorkerProc<WorkerArgs> for WorkerFn {
    async fn run(
        self,
        WorkerArgs { id, test_name }: WorkerArgs,
        init: SignalInit,
        done: WaitCompletion,
    ) {
        // Note that Rust's libtest does not consume output in subprocesses.
        // Disable logs to keep output compact.
        if !std::env::var("CUBESTORE_TEST_LOG_WORKER").is_ok() {
            *cubestore::config::TEST_LOGGING_INITIALIZED.write().await = true;
        }
        Config::test(&test_name)
            .update_config(|mut c| {
                c.select_worker_pool_size = 2;
                c.server_name = format!("localhost:{}", WORKER_PORTS[id]);
                c.worker_bind_address = Some(c.server_name.clone());
                c.metastore_remote_address = Some(format!("localhost:{}", METASTORE_PORT));
                c
            })
            .start_test_worker(|_| async move {
                init.signal().await;
                done.wait_completion().await;
            })
            .await
    }
}

#[cfg(target_os = "windows")]
fn main() {
    // We do not procspawn on Windows.
}
