//! Host-owned exclusive resource leases and serialized connection calls.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{Notify, OwnedMutexGuard, mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(30);
const PENDING: u8 = 0;
const HELD: u8 = 1;
const LOST: u8 = 2;
const RELEASED: u8 = 3;
// A waiting resource must not prevent another resource from acquiring the session.
const ACQUIRE_TURN: Duration = Duration::from_millis(100);

type LocalMap = Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>;
static LOCAL: OnceLock<LocalMap> = OnceLock::new();

#[derive(Debug)]
struct Lease {
    key: String,
    local: bool,
    connection: uuid::Uuid,
    active: AtomicBool,
    state: AtomicU8,
    epoch: AtomicU64,
    changed: Notify,
    calls: Arc<tokio::sync::Mutex<()>>,
}

impl Lease {
    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
        if state == LOST {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        self.changed.notify_waiters();
    }

    async fn wait(&self) -> Result<(), Error> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.active.load(Ordering::SeqCst) {
                return Err(Error {
                    connection: self.connection,
                });
            }
            match self.state.load(Ordering::SeqCst) {
                HELD => return Ok(()),
                LOST | RELEASED => {
                    return Err(Error {
                        connection: self.connection,
                    });
                }
                _ => changed.await,
            }
        }
    }
}

/// The connection cannot call its exclusive resource at this time.
#[derive(Debug)]
pub struct Error {
    connection: uuid::Uuid,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exclusive resource unavailable for connection {}",
            self.connection
        )
    }
}
impl std::error::Error for Error {}

/// One call holds the connection mutex until its integration returns.
#[derive(Debug)]
pub struct Call {
    lease: Arc<Lease>,
    _guard: OwnedMutexGuard<()>,
    epoch: u64,
}

impl Call {
    /// A lost database session also refuses the result of an unfinished call.
    pub fn held(&self) -> bool {
        self.lease.state.load(Ordering::SeqCst) == HELD
            && self.epoch == self.lease.epoch.load(Ordering::SeqCst)
    }
}

enum Command {
    Acquire(Arc<Lease>),
    Release(Arc<Lease>),
    Stop(oneshot::Sender<()>),
}

struct State {
    leases: HashMap<uuid::Uuid, Arc<Lease>>,
    revisions: HashMap<uuid::Uuid, u64>,
    watch_revision: u64,
    sender: Option<mpsc::UnboundedSender<Command>>,
    stopped: bool,
}

/// One lease owner per host, with a lazy dedicated system database session.
pub struct ResourceLeases {
    database: Option<String>,
    state: Mutex<State>,
}

impl std::fmt::Debug for ResourceLeases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceLeases").finish_non_exhaustive()
    }
}

impl Drop for ResourceLeases {
    fn drop(&mut self) {
        let state = self.state.get_mut().expect("resource lease state");
        for lease in state.leases.values() {
            lease.active.store(false, Ordering::SeqCst);
            lease.changed.notify_waiters();
        }
    }
}

impl ResourceLeases {
    pub fn new(url: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self {
            database: url.map(str::to_owned),
            state: Mutex::new(State {
                leases: HashMap::new(),
                revisions: HashMap::new(),
                watch_revision: 0,
                sender: None,
                stopped: false,
            }),
        })
    }

    /// Acquire on first call, then serialize every component on this connection.
    pub async fn call(
        &self,
        connection: uuid::Uuid,
        target: uuid::Uuid,
        resource: &str,
        local: bool,
    ) -> Result<Call, Error> {
        let lease = tokio::time::timeout(WAIT, self.enter(connection, target, resource, local))
            .await
            .unwrap_or(Err(Error { connection }))?;
        let guard = Arc::clone(&lease.calls).lock_owned().await;
        let epoch = lease.epoch.load(Ordering::SeqCst);
        if !lease.active.load(Ordering::SeqCst) || lease.state.load(Ordering::SeqCst) != HELD {
            return Err(Error { connection });
        }
        Ok(Call {
            lease,
            _guard: guard,
            epoch,
        })
    }

    async fn enter(
        &self,
        connection: uuid::Uuid,
        target: uuid::Uuid,
        resource: &str,
        local: bool,
    ) -> Result<Arc<Lease>, Error> {
        loop {
            let lease = {
                let mut state = self.state.lock().expect("resource lease state");
                if state.stopped {
                    return Err(Error { connection });
                }
                if state
                    .leases
                    .get(&connection)
                    .is_some_and(|lease| lease.state.load(Ordering::SeqCst) == RELEASED)
                {
                    state.leases.remove(&connection);
                }
                if let Some(lease) = state.leases.get(&connection) {
                    Arc::clone(lease)
                } else {
                    let lease = Arc::new(Lease {
                        key: format!("resource:{target}:{resource}"),
                        local,
                        connection,
                        active: AtomicBool::new(true),
                        state: AtomicU8::new(PENDING),
                        epoch: AtomicU64::new(0),
                        changed: Notify::new(),
                        calls: Arc::new(tokio::sync::Mutex::new(())),
                    });
                    if local {
                        tokio::spawn(local_lease(Arc::clone(&lease)));
                    } else {
                        let configuration = self.database.as_ref().ok_or(Error { connection })?;
                        let sender = state.sender.get_or_insert_with(|| {
                            let (sender, receiver) = mpsc::unbounded_channel();
                            tokio::spawn(database_session(configuration.clone(), receiver));
                            sender
                        });
                        sender
                            .send(Command::Acquire(Arc::clone(&lease)))
                            .map_err(|_| Error { connection })?;
                    }
                    state.leases.insert(connection, Arc::clone(&lease));
                    lease
                }
            };
            if !lease.active.load(Ordering::SeqCst) {
                let changed = lease.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if lease.state.load(Ordering::SeqCst) != RELEASED {
                    changed.await;
                }
                continue;
            }
            lease.wait().await?;
            return Ok(lease);
        }
    }

    /// Stop new calls immediately. The release waits for the current call.
    pub fn invalidate(&self, connection: uuid::Uuid) {
        let mut state = self.state.lock().expect("resource lease state");
        if let Some(revision) = state.revisions.get_mut(&connection) {
            *revision += 1;
        }
        if let Some(lease) = state.leases.get(&connection) {
            revoke(Arc::clone(lease), state.sender.clone());
        }
    }

    /// A broken fact watch stops calls until the database supplies new facts.
    pub fn clear(&self) {
        let mut state = self.state.lock().expect("resource lease state");
        state.watch_revision += 1;
        state.revisions.clear();
        for lease in state.leases.values() {
            revoke(Arc::clone(lease), state.sender.clone());
        }
    }

    pub(crate) fn revision(&self, connection: uuid::Uuid) -> (u64, u64) {
        let mut state = self.state.lock().expect("resource lease state");
        let revision = *state.revisions.entry(connection).or_default();
        (state.watch_revision, revision)
    }

    /// Drain calls, unlock resources, then close the dedicated session.
    pub async fn shutdown(&self) {
        let (leases, sender) = {
            let mut state = self.state.lock().expect("resource lease state");
            state.stopped = true;
            let leases: Vec<_> = state.leases.values().cloned().collect();
            for lease in &leases {
                revoke(Arc::clone(lease), state.sender.clone());
            }
            (leases, state.sender.take())
        };
        for lease in leases {
            loop {
                let changed = lease.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if lease.state.load(Ordering::SeqCst) == RELEASED {
                    break;
                }
                changed.await;
            }
        }
        if let Some(sender) = sender {
            let (done, wait) = oneshot::channel();
            let _ = sender.send(Command::Stop(done));
            let _ = wait.await;
        }
    }
}

fn revoke(lease: Arc<Lease>, sender: Option<mpsc::UnboundedSender<Command>>) {
    if !lease.active.swap(false, Ordering::SeqCst) {
        return;
    }
    lease.changed.notify_waiters();
    tokio::spawn(async move {
        let _guard = lease.calls.lock().await;
        if !lease.local
            && let Some(sender) = sender
            && sender.send(Command::Release(Arc::clone(&lease))).is_ok()
        {
            return;
        }
        lease.set(RELEASED);
    });
}

async fn local_lease(lease: Arc<Lease>) {
    let mutex = {
        let mut map = LOCAL
            .get_or_init(Mutex::default)
            .lock()
            .expect("local resource leases");
        map.retain(|_, entry| entry.strong_count() > 0);
        let mutex = map
            .get(&lease.key)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
        map.insert(lease.key.clone(), Arc::downgrade(&mutex));
        mutex
    };
    let changed = lease.changed.notified();
    tokio::pin!(changed);
    changed.as_mut().enable();
    let guard = tokio::select! {
        guard = mutex.lock_owned() => guard,
        () = &mut changed => { return; }
    };
    if !lease.active.load(Ordering::SeqCst) {
        return;
    }
    lease.set(HELD);
    loop {
        let changed = lease.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if lease.state.load(Ordering::SeqCst) == RELEASED {
            break;
        }
        if !lease.active.load(Ordering::SeqCst) {
            let _call = lease.calls.lock().await;
            break;
        }
        changed.await;
    }
    drop(guard);
    lease.set(RELEASED);
}

async fn database_session(configuration: String, mut commands: mpsc::UnboundedReceiver<Command>) {
    use sqlx::Connection as _;

    let mut needed: HashMap<uuid::Uuid, Arc<Lease>> = HashMap::new();
    let mut deferred = None;
    loop {
        while let Some(command) = deferred.take().or_else(|| commands.try_recv().ok()) {
            match command {
                Command::Acquire(lease) => {
                    needed.insert(lease.connection, lease);
                }
                Command::Release(lease) => {
                    needed.remove(&lease.connection);
                    lease.set(RELEASED);
                }
                Command::Stop(done) => {
                    let _ = done.send(());
                    return;
                }
            }
        }
        if needed.is_empty() {
            deferred = commands.recv().await;
            if deferred.is_none() {
                return;
            }
            continue;
        }
        let opened = tokio::select! {
            opened = wamn_postgres::connect(&configuration) => opened,
            command = commands.recv() => {
                let Some(command) = command else { return; };
                deferred = Some(command);
                continue;
            }
        };
        let mut client = match opened {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(%error, "resource lease session did not open");
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(1)) => {},
                    command = commands.recv() => {
                        let Some(command) = command else { return; };
                        deferred = Some(command);
                    }
                }
                continue;
            }
        };
        let mut pending: VecDeque<_> = needed
            .values()
            .filter(|lease| lease.active.load(Ordering::SeqCst))
            .map(Arc::clone)
            .collect();
        let mut poll = tokio::time::interval(ACQUIRE_TURN);
        loop {
            let command = if let Some(command) = deferred.take() {
                Some(command)
            } else {
                tokio::select! {
                    command = commands.recv() => command,
                    _ = poll.tick() => {
                        // A nonblocking lock needs no cancellation connection. One
                        // session owns every lock, including across retry turns.
                        let result = if let Some(lease) = pending.pop_front() {
                            if !lease.active.load(Ordering::SeqCst) { continue; }
                            if needed.values().any(|other| other.connection != lease.connection
                                && other.key == lease.key && other.state.load(Ordering::SeqCst) == HELD) {
                                continue;
                            }
                            let acquired = sqlx::query_scalar::<_, bool>(
                                "SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                                .bind(&lease.key).fetch_one(&mut client);
                            let result = tokio::select! {
                                result = tokio::time::timeout(WAIT, acquired) => result.ok().and_then(Result::ok),
                                command = commands.recv() => { deferred = command; break; }
                            };
                            match result {
                                Some(true) => lease.set(HELD),
                                Some(false) => pending.push_back(lease),
                                None => break,
                            }
                            Ok(())
                        } else {
                            // SQLx drives the socket during a query. Check held
                            // sessions too, so a disconnect invalidates their leases.
                            tokio::time::timeout(WAIT, client.ping()).await
                                .map_err(|_| ()).and_then(|result| result.map_err(|_| ()))
                        };
                        if result.is_err() { break; }
                        continue;
                    }
                }
            };
            match command {
                Some(Command::Acquire(lease)) => {
                    needed.insert(lease.connection, Arc::clone(&lease));
                    pending.push_back(lease);
                }
                Some(Command::Release(lease)) => {
                    if lease.state.load(Ordering::SeqCst) == HELD
                        && sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
                            .bind(&lease.key)
                            .execute(&mut client)
                            .await
                            .is_err()
                    {
                        deferred = Some(Command::Release(lease));
                        break;
                    }
                    needed.remove(&lease.connection);
                    lease.set(RELEASED);
                    for waiting in needed.values().filter(|waiting| {
                        waiting.key == lease.key && waiting.active.load(Ordering::SeqCst)
                    }) {
                        if !pending.iter().any(|entry| Arc::ptr_eq(entry, waiting)) {
                            pending.push_back(Arc::clone(waiting));
                        }
                    }
                }
                Some(Command::Stop(done)) => {
                    let _ = client.close().await;
                    let _ = done.send(());
                    return;
                }
                None => return,
            }
        }
        // Closing the session releases its locks before reconnecting.
        for lease in needed.values() {
            if lease.state.load(Ordering::SeqCst) != RELEASED {
                lease.set(LOST);
            }
        }
        drop(client);
    }
}

#[cfg(test)]
mod tests;
