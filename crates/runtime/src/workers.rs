//! Threads that run what would block the runtime thread: completions and
//! prefills, Reply & rewrite generations, and conversation reads. Each takes
//! jobs in order from its own channel and answers on the runtime's.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use ghostcaret_engine::complete::{
    CancelToken, CompleteError, Completer, Completion, CompletionRequest, GenerateRequest,
    Generator,
};
use ghostcaret_engine::controller::{GenerateUpdate, Input};
use ghostcaret_engine::io::{Backend, FieldId};
use ghostcaret_engine::stop::Stopper;

/// Results from the workers.
pub(crate) enum WorkerMsg {
    Completed {
        id: u64,
        result: Result<Completion, CompleteError>,
        queue: Duration,
        decode: Duration,
    },
    Input(Input),
}

pub(crate) enum Job {
    Complete {
        request: CompletionRequest,
        model: Arc<dyn Completer>,
        cancel: CancelToken,
        queued: Instant,
    },
    Prefill {
        request: CompletionRequest,
        model: Arc<dyn Completer>,
        cancel: CancelToken,
    },
}

pub(crate) struct GenJob {
    pub request: GenerateRequest,
    pub model: Arc<dyn Generator>,
    pub cancel: CancelToken,
}

/// The completion and generation workers, started before the runtime thread
/// so a failure to start them reaches the host.
pub(crate) struct Workers {
    pub jobs: Sender<Job>,
    pub gens: Sender<GenJob>,
    pub threads: [JoinHandle<()>; 2],
    pub tx: Sender<WorkerMsg>,
    pub rx: Receiver<WorkerMsg>,
}

impl Workers {
    pub(crate) fn start() -> io::Result<Workers> {
        let (tx, rx) = unbounded();
        let (jobs, jobs_rx) = unbounded();
        let (gens, gens_rx) = unbounded();
        let complete = {
            let tx = tx.clone();
            thread::Builder::new()
                .name("ghostcaret-complete".into())
                .spawn(move || complete(&jobs_rx, &tx))?
        };
        let generate = {
            let tx = tx.clone();
            thread::Builder::new()
                .name("ghostcaret-generate".into())
                .spawn(move || generate(&gens_rx, &tx))?
        };
        Ok(Workers {
            jobs,
            gens,
            threads: [complete, generate],
            tx,
            rx,
        })
    }
}

/// Runs one job at a time; the controller keeps at most one completion in
/// flight, and cancels it on the next edit.
fn complete(jobs: &Receiver<Job>, out: &Sender<WorkerMsg>) {
    for job in jobs {
        match job {
            Job::Complete {
                request,
                model,
                cancel,
                queued,
            } => {
                let started = Instant::now();
                let result = if cancel.is_cancelled() {
                    Err(CompleteError::Cancelled)
                } else {
                    let mut stop = Stopper::new(&request.stop);
                    model.complete(&request, &mut stop, &cancel)
                };
                let msg = WorkerMsg::Completed {
                    id: request.id,
                    result,
                    queue: started - queued,
                    decode: started.elapsed(),
                };
                if out.send(msg).is_err() {
                    return;
                }
            }
            // A prefill with newer work queued behind it would only delay it.
            Job::Prefill {
                request,
                model,
                cancel,
            } => {
                if !cancel.is_cancelled() && jobs.is_empty() {
                    model.prefill(&request, &cancel);
                }
            }
        }
    }
}

fn generate(jobs: &Receiver<GenJob>, out: &Sender<WorkerMsg>) {
    for GenJob {
        request,
        model,
        cancel,
    } in jobs
    {
        let id = request.id;
        let result = if cancel.is_cancelled() {
            Err(CompleteError::Cancelled)
        } else {
            model.generate(&request, &cancel, &mut |delta| {
                let update = GenerateUpdate::Delta(delta.to_owned());
                let _ = out.send(WorkerMsg::Input(Input::Generated { id, update }));
            })
        };
        let done = WorkerMsg::Input(Input::Generated {
            id,
            update: GenerateUpdate::Done(result),
        });
        if out.send(done).is_err() {
            return;
        }
    }
}

/// Starts the conversation reader; [`Backend::conversation`] may block.
pub(crate) fn conversation_reader(
    backend: Arc<dyn Backend>,
    out: Sender<WorkerMsg>,
) -> io::Result<Sender<FieldId>> {
    let (tx, rx) = unbounded::<FieldId>();
    thread::Builder::new()
        .name("ghostcaret-conversation".into())
        .spawn(move || {
            for field in rx {
                let conversation = backend.conversation(field);
                if out
                    .send(WorkerMsg::Input(Input::Conversation {
                        field,
                        conversation,
                    }))
                    .is_err()
                {
                    return;
                }
            }
        })?;
    Ok(tx)
}
