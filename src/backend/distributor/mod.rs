// Copyright (c) 2018 Nuclear Furnace
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
mod random;
mod modulo;
pub use self::random::RandomDistributor;
pub use self::modulo::ModuloDistributor;

/// A placeholder for backends.  This lets us avoid holding references to the actual backends.
pub struct BackendDescriptor;

impl BackendDescriptor {
    pub fn new() -> BackendDescriptor { BackendDescriptor {} }
}

/// Distributes items amongst a set of backends.
///
/// After being seeded with a set of backends, one of them can be chosen by mapping a point amongst
/// them.  This could be by modulo division (point % backend count), libketama, or others.
pub trait Distributor {
    fn seed(&mut self, backends: Vec<BackendDescriptor>);

    /// Chooses a backend based on the given point.
    ///
    /// The return value is the list position, zero-indexed, based on the list of backends given to
    /// `seed`.
    fn choose(&self, point: u64) -> usize;
}

pub fn configure_distributor(dist_type: &str) -> Box<Distributor + Send + Sync> {
    match dist_type {
        "random" => Box::new(RandomDistributor::new()),
        "modulo" => Box::new(ModuloDistributor::new()),
        s => panic!("unknown distributor type {}", s),
    }
}
