#![feature(test)]
#![feature(iterator_flatten)]
#![recursion_limit = "1024"]

extern crate config;
extern crate crypto;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

extern crate chan;
extern crate chan_signal;

use chan_signal::Signal;

extern crate tokio;
extern crate tokio_io;
#[macro_use]
extern crate futures;
extern crate net2;
extern crate rs_futures_spmc;

use rs_futures_spmc::channel;
use std::thread;
use tokio::prelude::*;
use tokio::reactor::Handle;

#[macro_use]
extern crate log;
#[macro_use(slog_o, slog_kv)]
extern crate slog;
extern crate slog_async;
extern crate slog_scope;
extern crate slog_stdlog;
extern crate slog_term;

use slog::Drain;

extern crate atoi;
extern crate bytes;
extern crate itoa;
extern crate rand;

#[cfg(test)]
extern crate test;

#[cfg(test)]
extern crate spectral;

mod backend;
mod conf;
mod listener;
mod protocol;
mod util;

use conf::Configuration;
use conf::LevelExt;

fn main() {
    // Due to the way signal masking apparently works, or works with this library, we
    // must initialize our signal handling code before *any* threads are spun up by
    // the process, otherwise we don't seem to get them delivered to us.
    //
    // We also have this accessory thread because trying to wrap the channel as a stream
    // was fraught with pain and this is much simpler.  C'est la vie.
    let signals = chan_signal::notify(&[Signal::USR1, Signal::INT]);
    let (close_tx, close_rx) = channel::<()>(1);
    thread::spawn(move || {
        loop {
            let signal = signals.recv().unwrap();
            info!("[core] signal received: {:?}", signal);

            match signal {
                Signal::USR1 => {} // signal to spawn new process
                Signal::INT => {
                    // signal to close this process
                    let _ = close_tx.send(()).wait();
                    break;
                }
                _ => {} // we don't care about the rest
            }
        }
    });

    let configuration = Configuration::new().expect("failed to parse configuration");

    // Configure our logging.  This gives us fully asynchronous logging to the terminal
    // which is also level filtered.  As well, we've replaced the global std logger
    // and pulled in helper macros that correspond to the various logging levels.
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let logger = slog::Logger::root(
        slog::LevelFilter::new(drain, slog::Level::from_str(&configuration.logging.level)).fuse(),
        slog_o!("version" => env!("CARGO_PKG_VERSION")),
    );

    let _scope_guard = slog_scope::set_global_logger(logger);
    let _log_guard = slog_stdlog::init().unwrap();
    info!("[core] logging configured");

    // Now run.
    tokio::run(future::lazy(move || {
        for pool_config in configuration.pools {
            let close = close_rx.clone();
            let config = pool_config.clone();
            let reactor = Handle::current();

            let pool = pool::from_config(reactor, config, close);
            tokio::spawn(pool);

            info!("[pool] starting listening '{}'", pool_config.address);
        }

        info!("[core] synchrotron running");

        Ok(())
    }))
}
