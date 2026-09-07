//! Runtime selection at materialization. Both engines use the same pool,
//! request driver, admission, storage ownership, and durability gates.
use super::*;

pub enum Worker {
    JavaScript(JsWorker),
    Monty(monty_worker::Worker),
}

macro_rules! dispatch {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self {
            Self::JavaScript(worker) => worker.$method($($arg),*),
            Self::Monty(worker) => worker.$method($($arg),*),
        }
    };
}

impl Worker {
    pub fn load_config(config: Arc<WorkerConfig>) -> Result<Self> {
        if config.src.starts_with(crate::python::MAGIC) {
            Ok(Self::Monty(monty_worker::Worker::load_config(config)?))
        } else {
            Ok(Self::JavaScript(JsWorker::load_config(config)?))
        }
    }
    #[cfg(celld_internal_tests)]
    pub fn load(options: WorkerConfigOptions) -> Result<Self> {
        Self::load_config(Arc::new(WorkerConfig::new(options)))
    }
    pub fn turn_begin(
        &mut self,
        job: crate::WorkerJob,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> (Option<InFlight>, Vec<Op>) {
        dispatch!(self, turn_begin, job, trace)
    }
    pub fn turn_begin_cell(
        &mut self,
        job: CellJob,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> (Option<InFlight>, Vec<Op>) {
        dispatch!(self, turn_begin_cell, job, trace)
    }
    pub fn turn_deliver(
        &mut self,
        entry: &mut InFlight,
        op: u64,
        result: Result<asyncrt::OpOut, String>,
    ) -> Vec<Op> {
        dispatch!(self, turn_deliver, entry, op, result)
    }
    pub fn turn_cancel(&mut self, entry: &mut InFlight) -> Vec<Op> {
        dispatch!(self, turn_cancel, entry)
    }
    pub fn turn_poll(&mut self, entry: &mut InFlight) -> Vec<Op> {
        dispatch!(self, turn_poll, entry)
    }
    pub fn turn_finish_alarm(&mut self, entry: &mut InFlight) {
        dispatch!(self, turn_finish_alarm, entry)
    }
    pub fn own_cell(
        &mut self,
        cell: &str,
        storage: Option<CellStorage<'_>>,
    ) -> Result<Option<i64>> {
        dispatch!(self, own_cell, cell, storage)
    }
    pub fn set_id_name(&mut self, scope: &str, name: &str) -> Result<()> {
        dispatch!(self, set_id_name, scope, name)
    }
    pub fn take_alarm_moves(&mut self) -> Vec<(String, i64)> {
        dispatch!(self, take_alarm_moves)
    }
}
