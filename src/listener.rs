use backend::distributor::Distributor;
use backend::hasher::Hasher;
use backend::pool::BackendPool;
use backend::redis::generate_batched_writes;
use backend::{distributor, hasher};
use conf::ListenerConfiguration;
use futures::future::{join_all, lazy, ok};
use futures::prelude::*;
use net2::TcpBuilder;
use protocol::redis;
use rs_futures_spmc::Receiver;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio;
use tokio::io;
use tokio::net::TcpListener;
use tokio::reactor::Handle;
use tokio_io::AsyncRead;
use util::{flatten_ordered_messages, StreamExt};

type GenericRuntimeFuture = Box<Future<Item = (), Error = ()> + Sync + Send + 'static>;

/// Creates a listener from the given configuration.
///
/// The listener will spawn a socket for accepting client connections, and when a client connects,
/// spawn a task to process all of the messages from that client until the client disconnects or
/// there is an unrecoverable connection/protocol error.
pub fn from_config(
    reactor: Handle,
    config: ListenerConfiguration,
    close: Receiver<()>,
) -> Result<GenericRuntimeFuture, Error> {
    // Create the actual listener proper.
    let listen_address = config.address.clone();
    let listener =
        get_listener(&listen_address, &reactor).expect("failed to create the TCP listener");

    // Gather up all of the backend pools.
    let mut pools = HashMap::new();
    let pool_configs = config.pools.clone();
    for (pool_name, mut pool_config) in pool_configs {
        let dist_type = pool_config
            .options
            .entry("distribution".to_owned())
            .or_insert("random".to_owned())
            .to_lowercase();
        let distributor = distributor::configure_distributor(dist_type);

        let hash_type = pool_config
            .options
            .entry("hash".to_owned())
            .or_insert("md5".to_owned())
            .to_lowercase();
        let hasher = hasher::configure_hasher(hash_type);

        let pool = Arc::new(BackendPool::new(pool_config.addresses, distributor, hasher));
        pools.insert(pool_name, pool);
    }

    // Get the correct handler based on protocol.
    let protocol = config.protocol.to_lowercase();
    let handler = match protocol.as_str() {
        "redis" => redis_from_config(config, listener, pools)?,
        s => panic!("unknown protocol type: {}", s),
    };

    // Make sure our handlers close out when told.
    let listen_address2 = listen_address.clone();
    let wrapped = lazy(move || {
        info!("[listener] starting listener '{}'...", listen_address);
        ok(())
    }).and_then(|_| handler)
        .select2(close.into_future())
        .then(move |_| {
            info!("[pool] shutting down listener '{}'", listen_address2);
            ok(())
        });
    Ok(Box::new(wrapped))
}

fn redis_from_config<D, H>(
    config: ListenerConfiguration,
    listener: TcpListener,
    pools: HashMap<String, Arc<BackendPool<D, H>>>,
) -> Result<GenericRuntimeFuture, Error>
where
    D: Distributor + Send + Sync + 'static,
    H: Hasher + Send + Sync + 'static,
{
    // Figure out what sort of routing we're doing so we can grab the right handler.
    let routing_type = config.routing.to_lowercase();
    match routing_type.as_str() {
        "warmup" => redis_warmup_handler(listener, pools),
        _ => redis_normal_handler(listener, pools),
    }
}

fn redis_warmup_handler<D, H>(
    listener: TcpListener,
    pools: HashMap<String, Arc<BackendPool<D, H>>>,
) -> Result<GenericRuntimeFuture, Error>
where
    D: Distributor + Send + Sync + 'static,
    H: Hasher + Send + Sync + 'static,
{
    let warm_pool = pools
        .get("warm")
        .ok_or(Error::new(
            ErrorKind::Other,
            "redis warmup handler has no 'warm' pool configured!",
        ))?
        .clone();

    let cold_pool = pools
        .get("cold")
        .ok_or(Error::new(
            ErrorKind::Other,
            "redis warmup handler has no 'cold' pool configured!",
        ))?
        .clone();

    let handler = listener
        .incoming()
        .map_err(|e| error!("[pool] accept failed: {:?}", e))
        .for_each(move |socket| {
            let client_addr = socket.peer_addr().unwrap();
            debug!("[client] connection established -> {:?}", client_addr);

            let cold = cold_pool.clone();
            let warm = warm_pool.clone();

            let (client_rx, client_tx) = socket.split();
            let client_proto = redis::read_messages_stream(client_rx)
                .map_err(|e| {
                    error!("[client] caught error while reading from client: {:?}", e);
                })
                .batch(128)
                .fold(client_tx, move |tx, msgs| {
                    trace!("[client] got batch of {} messages!", msgs.len());

                    // Fire off our cold pool operations asynchronously so that we don't influence
                    // the normal client path.
                    let cold_msgs = msgs.clone();
                    let cold_batches = generate_batched_writes(&cold, cold_msgs);
                    let cold_handler = join_all(cold_batches)
                        .map_err(|err| {
                            error!(
                                "[client] error while sending warming ops to cold pool: {:?}",
                                err
                            )
                        })
                        .map(|_| ());

                    // Now run our normal writes.
                    let warm_handler = join_all(generate_batched_writes(&warm, msgs))
                        .and_then(|results| ok(flatten_ordered_messages(results)))
                        .and_then(move |items| redis::write_messages(tx, items))
                        .map(|(w, _n)| w)
                        .map_err(|err| {
                            error!("[client] caught error while handling request: {:?}", err)
                        });

                    warm_handler.join(cold_handler).map(|(a, _)| a)
                })
                .map(|_| ());

            tokio::spawn(client_proto)
        });

    Ok(Box::new(handler))
}

fn redis_normal_handler<D, H>(
    listener: TcpListener,
    pools: HashMap<String, Arc<BackendPool<D, H>>>,
) -> Result<GenericRuntimeFuture, Error>
where
    D: Distributor + Send + Sync + 'static,
    H: Hasher + Send + Sync + 'static,
{
    let default_pool = pools
        .get("default")
        .ok_or(Error::new(
            ErrorKind::Other,
            "redis normal handler has no 'default' pool configured!",
        ))?
        .clone();

    let handler = listener
        .incoming()
        .map_err(|e| error!("[pool] accept failed: {:?}", e))
        .for_each(move |socket| {
            let client_addr = socket.peer_addr().unwrap();
            info!("[client] connection established -> {:?}", client_addr);

            let default = default_pool.clone();

            let (client_rx, client_tx) = socket.split();
            let client_proto = redis::read_messages_stream(client_rx)
                .map_err(|e| {
                    error!("[client] caught error while reading from client: {:?}", e);
                })
                .batch(128)
                .fold((client_tx, client_addr), move |(tx, addr), msgs| {
                    trace!(
                        "[client] [{:?}] got batch of {} messages!",
                        addr,
                        msgs.len()
                    );

                    join_all(generate_batched_writes(&default, msgs))
                        .and_then(|results| ok(flatten_ordered_messages(results)))
                        .and_then(move |items| redis::write_messages(tx, items))
                        .map(move |(w, _n)| {
                            trace!("[client] [{:?}] sent batch of responses to client", addr);
                            (w, addr)
                        })
                        .map_err(|err| {
                            error!("[client] caught error while handling request: {:?}", err)
                        })
                })
                .map(|(_, addr)| {
                    info!("[client] connection complete -> {:?}", addr);
                    ()
                });

            tokio::spawn(client_proto)
        });

    Ok(Box::new(handler))
}

fn get_listener(addr_str: &String, handle: &Handle) -> io::Result<TcpListener> {
    let addr = addr_str.parse().unwrap();
    let builder = match addr {
        SocketAddr::V4(_) => TcpBuilder::new_v4()?,
        SocketAddr::V6(_) => TcpBuilder::new_v6()?,
    };
    configure_builder(&builder)?;
    builder.reuse_address(true)?;
    builder.bind(addr)?;
    builder
        .listen(1024)
        .and_then(|l| TcpListener::from_std(l, handle))
}

#[cfg(unix)]
fn configure_builder(builder: &TcpBuilder) -> io::Result<()> {
    use net2::unix::*;

    builder.reuse_port(true)?;
    Ok(())
}

#[cfg(windows)]
fn configure_builder(_builder: &TcpBuilder) -> io::Result<()> {
    Ok(())
}
