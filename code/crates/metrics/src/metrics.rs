use std::fmt::Write;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cadence::{Counted, StatsdClient, Timed};
use malachitebft_core_state_machine::state::Step;
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, linear_buckets, Histogram};

#[derive(Clone, Debug)]
pub struct Metrics {
    inner: Arc<Inner>,
    statsd_client: Option<Arc<StatsdClient>>,
    shard_id: Option<u32>,
    use_tags: bool,
}

impl Deref for Metrics {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Label set for the `time_per_step` metric.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TimePerStep {
    step: AsLabelValue<Step>,
}

impl TimePerStep {
    pub fn new(step: Step) -> Self {
        Self {
            step: AsLabelValue(step),
        }
    }
}

/// This wrapper allows us to derive `AsLabelValue` for `Step` without
/// running into Rust orphan rules, cf. <https://rust-lang.github.io/chalk/book/clauses/coherence.html>
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct AsLabelValue<T>(T);

impl EncodeLabelValue for AsLabelValue<Step> {
    fn encode(
        &self,
        encoder: &mut prometheus_client::encoding::LabelValueEncoder,
    ) -> Result<(), std::fmt::Error> {
        encoder.write_fmt(format_args!("{:?}", self.0))
    }
}

#[derive(Clone, Debug)]
pub struct Inner {
    /// Number of blocks finalized
    finalized_blocks: Counter,

    /// Number of transactions finalized
    pub finalized_txes: Counter,

    /// Consensus time, in seconds
    consensus_time: Histogram,

    /// Time taken to finalize a block, in seconds
    time_per_block: Histogram,

    /// Time taken for a step within a round, in secodns
    pub time_per_step: Family<TimePerStep, Histogram>,

    /// Block size in terms of # of transactions
    pub block_tx_count: Histogram,

    /// Size of each block in bytes
    pub block_size_bytes: Histogram,

    /// The consensus round in which the node was when it finalized a block
    pub consensus_round: Histogram,

    /// The round of the proposal that was decided on
    pub proposal_round: Histogram,

    /// Number of times consensus was blocked in Prevote or Precommit step and required vote synchronization
    step_timeouts: Counter,

    /// Number of connected peers, ie. for each consensus node, how many peers is it connected to)
    connected_peers: Gauge,

    /// Current height
    pub height: Gauge,

    /// Current round
    pub round: Gauge,

    /// Time taken to sign a message
    pub signature_signing_time: Histogram,

    /// Time taken to verify a signature
    pub signature_verification_time: Histogram,

    /// Internal state for measuring time taken for consensus
    instant_consensus_started: Arc<AtomicInstant>,

    /// Internal state for measuring time taken to finalize a block
    instant_block_started: Arc<AtomicInstant>,

    /// Internal state for measuring time taken for a step within a round
    instant_step_started: Arc<Mutex<(Step, Instant)>>,
}

impl Metrics {
    pub fn new(
        shard_id: Option<u32>,
        statsd_client: Option<Arc<StatsdClient>>,
        use_tags: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                finalized_blocks: Counter::default(),
                finalized_txes: Counter::default(),
                consensus_time: Histogram::new(linear_buckets(0.0, 0.1, 20)),
                time_per_block: Histogram::new(linear_buckets(0.0, 0.1, 20)),
                time_per_step: Family::new_with_constructor(|| {
                    Histogram::new(linear_buckets(0.0, 0.1, 20))
                }),
                block_tx_count: Histogram::new(linear_buckets(0.0, 32.0, 128)),
                block_size_bytes: Histogram::new(linear_buckets(0.0, 64.0 * 1024.0, 128)),
                consensus_round: Histogram::new(linear_buckets(0.0, 1.0, 20)),
                proposal_round: Histogram::new(linear_buckets(0.0, 1.0, 20)),
                step_timeouts: Counter::default(),
                connected_peers: Gauge::default(),
                height: Gauge::default(),
                round: Gauge::default(),
                signature_signing_time: Histogram::new(exponential_buckets(0.001, 2.0, 10)),
                signature_verification_time: Histogram::new(exponential_buckets(0.001, 2.0, 10)),
                instant_consensus_started: Arc::new(AtomicInstant::empty()),
                instant_block_started: Arc::new(AtomicInstant::empty()),
                instant_step_started: Arc::new(Mutex::new((Step::Unstarted, Instant::now()))),
            }),
            statsd_client,
            shard_id,
            use_tags,
        }
    }

    pub fn register(
        registry: &SharedRegistry,
        shard_id: Option<u32>,
        statsd_client: Option<Arc<StatsdClient>>,
        use_tags: bool,
    ) -> Self {
        let metrics = Self::new(shard_id, statsd_client, use_tags);

        registry.with_prefix("malachitebft_core_consensus", |registry| {
            registry.register(
                "finalized_blocks",
                "Number of blocks finalized",
                metrics.finalized_blocks.clone(),
            );

            registry.register(
                "finalized_txes",
                "Number of transactions finalized",
                metrics.finalized_txes.clone(),
            );

            registry.register(
                "consensus_time",
                "Consensus time, in seconds",
                metrics.consensus_time.clone(),
            );

            registry.register(
                "time_per_block",
                "Time taken to finalize a block, in seconds",
                metrics.time_per_block.clone(),
            );

            registry.register(
                "time_per_step",
                "Time taken for a step in a round, in seconds",
                metrics.time_per_step.clone(),
            );

            registry.register(
                "block_tx_count",
                "Block size in terms of # of transactions",
                metrics.block_tx_count.clone(),
            );

            registry.register(
                "block_size_bytes",
                "Size of each block in bytes",
                metrics.block_size_bytes.clone(),
            );

            registry.register(
                "consensus_round",
                "The consensus round in which the node was when it finalized a block",
                metrics.consensus_round.clone(),
            );

            registry.register(
                "proposal_round",
                "The round of the proposal that was decided on",
                metrics.proposal_round.clone(),
            );

            registry.register(
                "step_timeouts",
                "Number of times consensus was blocked and required vote synchronization",
                metrics.step_timeouts.clone(),
            );

            registry.register(
                "connected_peers",
                "Number of connected peers, ie. for each consensus node, how many peers is it connected to",
                metrics.connected_peers.clone(),
            );

            registry.register(
                "height",
                "Current height",
                metrics.height.clone(),
            );

            registry.register(
                "round",
                "Current round",
                metrics.round.clone(),
            );

            registry.register(
                "signature_signing_time",
                "Time taken to sign a message, in seconds",
                metrics.signature_signing_time.clone(),
            );

            registry.register(
                "signature_verification_time",
                "Time taken to verify a signature, in seconds",
                metrics.signature_verification_time.clone(),
            );
        });

        metrics
    }

    fn count_with_shard(&self, key: &str, count: i64) {
        match &self.statsd_client {
            None => {}
            Some(statsd_client) => match self.shard_id {
                None => {
                    let _ =
                        statsd_client.count(format!("malachite.consensus.{}", key).as_str(), count);
                }
                Some(shard_id) => {
                    if self.use_tags {
                        statsd_client
                            .count_with_tags(format!("malachite.consensus.{}", key).as_str(), count)
                            .with_tag("shard", format!("{}", shard_id).as_str())
                            .send();
                    } else {
                        let _ = statsd_client.count(
                            format!("shard{}.malachite.consensus.{}", shard_id, key).as_str(),
                            count,
                        );
                    }
                }
            },
        }
    }

    fn time_with_shard(&self, key: &str, count: u64) {
        match &self.statsd_client {
            None => {}
            Some(statsd_client) => match self.shard_id {
                None => {
                    let _ =
                        statsd_client.time(format!("malachite.consensus.{}", key).as_str(), count);
                }
                Some(shard_id) => {
                    if self.use_tags {
                        statsd_client
                            .time_with_tags(format!("malachite.consensus.{}", key).as_str(), count)
                            .with_tag("shard", format!("{}", shard_id).as_str())
                            .send();
                    } else {
                        let _ = statsd_client.time(
                            format!("shard{}.malachite.consensus.{}", shard_id, key).as_str(),
                            count,
                        );
                    }
                }
            },
        }
    }

    pub fn finalized_block(&self) {
        self.finalized_blocks.inc();
        self.count_with_shard("finalized_block", 1);
    }

    pub fn peer_connected(&self) {
        self.connected_peers.inc();
        self.count_with_shard("connected_peers", 1);
    }

    pub fn peer_disconnected(&self) {
        self.connected_peers.dec();
        self.count_with_shard("connected_peers", -1);
    }

    pub fn consensus_start(&self) {
        self.instant_consensus_started.set_now();
    }

    pub fn consensus_end(&self) {
        if !self.instant_consensus_started.is_empty() {
            let elapsed = self.instant_consensus_started.elapsed().as_secs_f64();
            self.consensus_time.observe(elapsed);

            self.instant_consensus_started.set_millis(0);
            self.time_with_shard(
                "consensus_time",
                self.instant_consensus_started.elapsed().as_millis() as u64,
            );
        }
    }

    pub fn block_start(&self) {
        self.instant_block_started.set_now();
    }

    pub fn block_end(&self) {
        if !self.instant_block_started.is_empty() {
            let elapsed = self.instant_block_started.elapsed().as_secs_f64();
            self.time_per_block.observe(elapsed);
            self.time_with_shard(
                "block_time",
                self.instant_block_started.elapsed().as_millis() as u64,
            );

            self.instant_block_started.set_millis(0);
        }
    }

    pub fn step_timeout(&self) {
        self.step_timeouts.inc();
        self.count_with_shard("step_timeouts", 1);
    }

    pub fn step_start(&self, step: Step) {
        let mut guard = self.instant_step_started.lock().expect("poisoned mutex");
        *guard = (step, Instant::now());
    }

    pub fn step_end(&self, step: Step) {
        let mut guard = self.instant_step_started.lock().expect("poisoned mutex");

        let (current_step, started) = *guard;
        debug_assert_eq!(current_step, step, "step_end called for wrong step");

        // If the step was never started, ignore
        if current_step == Step::Unstarted {
            return;
        }

        self.time_per_step
            .get_or_create(&TimePerStep::new(step))
            .observe(started.elapsed().as_secs_f64());

        *guard = (Step::Unstarted, Instant::now());
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(None, None, false)
    }
}

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use crate::SharedRegistry;

#[derive(Default, Debug)]
struct AtomicInstant(AtomicU64);

#[allow(dead_code)]
impl AtomicInstant {
    pub fn now() -> Self {
        Self(AtomicU64::new(Self::now_millis()))
    }

    pub fn empty() -> Self {
        Self(AtomicU64::new(0))
    }

    pub const fn from_millis(millis: u64) -> Self {
        Self(AtomicU64::new(millis))
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(Self::now_millis() - self.as_millis())
    }

    pub fn as_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    pub fn set_now(&self) {
        self.set_millis(Self::now_millis());
    }

    pub fn set_millis(&self, millis: u64) {
        self.0.store(millis, Ordering::SeqCst);
    }

    pub fn is_empty(&self) -> bool {
        self.as_millis() == 0
    }

    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}
