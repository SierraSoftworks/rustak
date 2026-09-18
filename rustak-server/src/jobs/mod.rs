//! Background work: the queue consumer and the jobs it runs.
//!
//! A job owns a queue partition and is registered at link time with
//! [`register_job!`](crate::register_job), so writing one and having it run are
//! the same act — there is no list to keep in step. [`JobHost::run`] builds the
//! registry, gives every job its one-time [`setup`](Job::setup), and then
//! consumes the queue until the server is asked to stop.
//!
//! # The shape of a job
//!
//! ```
//! use rustak_server::prelude::*;
//! use rustak_server::register_job;
//!
//! #[derive(Serialize, Deserialize)]
//! struct Greeting {
//!     name: String,
//! }
//!
//! struct GreetJob;
//! register_job!(GreetJob);
//!
//! impl Job for GreetJob {
//!     type JobType = Greeting;
//!
//!     fn partition() -> &'static str {
//!         "example/greet"
//!     }
//!
//!     async fn handle(
//!         &self,
//!         ctx: JobContext<impl Services + Send + Sync + 'static>,
//!         job: &Self::JobType,
//!     ) -> Result<(), human_errors::Error> {
//!         ctx.services().kv().set("example", "greeted", job.name.clone()).await
//!     }
//! }
//! ```
//!
//! # Recurring work
//!
//! There is no scheduler. A job that runs periodically arms itself in
//! [`setup`](Job::setup) and re-arms itself at the top of
//! [`handle`](Job::handle), enqueueing under a fixed idempotency key so the
//! schedule can never be armed twice. That re-arm also clears the reservation
//! the host holds, so the `complete` which follows a successful run removes
//! nothing and the rescheduled message survives — which is why the re-arm comes
//! first, and why a run that fails is one missed run rather than the end of the
//! schedule. [`AuditPruneJob`] and [`WalCheckpointJob`] are both of this shape.

pub mod acme_renew;
pub mod audit_prune;
pub mod content_orphans;
pub mod dead_letter;
pub mod host;
pub mod job;
pub mod mission_expiry;
pub mod retention;
pub mod runnable;
pub mod service_health;
pub mod tls_files;
pub mod wal_checkpoint;

pub use acme_renew::{
    ACME_RENEW_FORCED_KEY, ACME_RENEW_PARTITION, AcmeRenewJob, AcmeRenewTask, RENEW_INTERVAL,
};
pub use audit_prune::{AUDIT_PRUNE_PARTITION, AuditPruneJob, AuditPruneTask, PRUNE_INTERVAL};
pub use content_orphans::{
    CONTENT_ORPHANS_PARTITION, ContentOrphansJob, ContentOrphansTask,
    SWEEP_INTERVAL as CONTENT_ORPHANS_INTERVAL,
};
pub use dead_letter::DEAD_LETTERS;
pub use host::JobHost;
pub use job::{DEFAULT_JOB_TIMEOUT, Job, JobContext};
pub use mission_expiry::{
    EXPIRY_INTERVAL, MISSION_EXPIRY_PARTITION, MissionExpiryJob, MissionExpiryTask,
};
pub use retention::{COT_RETENTION_PARTITION, CotRetentionJob, CotRetentionTask, SWEEP_INTERVAL};
pub use runnable::{JobRegistration, JobRunnable};
pub use service_health::{
    SERVICE_HEALTH_INTERVAL, SERVICE_HEALTH_PARTITION, ServiceHealthJob, ServiceHealthTask,
};
pub use tls_files::{TLS_FILES_PARTITION, TLS_FILES_RELOAD_KEY, TlsFilesJob, TlsFilesTask};
pub use wal_checkpoint::{WAL_CHECKPOINT_PARTITION, WalCheckpointJob, WalCheckpointTask};
