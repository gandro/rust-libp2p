// Copyright 2017 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Implementation of the `/ipfs/id/1.0.0` protocol. Allows a node A to query another node B which
//! information B knows about A. Also includes the addresses B is listening on.
//!
//! When two nodes connect to each other, the listening half sends a message to the dialing half,
//! indicating the information, and then the protocol stops.

extern crate bytes;
extern crate futures;
extern crate multiaddr;
extern crate libp2p_peerstore;
extern crate libp2p_swarm;
extern crate protobuf;
extern crate tokio_io;
extern crate varint;

use bytes::{Bytes, BytesMut};
use futures::{Future, Stream, Sink};
use libp2p_swarm::{ConnectionUpgrade, Endpoint};
use multiaddr::Multiaddr;
use protobuf::Message as ProtobufMessage;
use protobuf::core::parse_from_bytes as protobuf_parse_from_bytes;
use protobuf::repeated::RepeatedField;
use std::io::{Error as IoError, ErrorKind as IoErrorKind};
use std::iter;
use tokio_io::{AsyncRead, AsyncWrite};
use varint::VarintCodec;

mod structs_proto;

/// Prototype for an upgrade to the identity protocol.
#[derive(Debug, Clone)]
pub struct IdentifyProtocol {
	/// Our public key to report to the remote.
	pub public_key: Vec<u8>,
	/// Version of the "global" protocol, eg. `ipfs/1.0.0` or `polkadot/1.0.0`.
	pub protocol_version: String,
	/// Name and version of the client. Can be thought as similar to the `User-Agent` header
	/// of HTTP.
	pub agent_version: String,
	/// Addresses that we are listening on.
	pub listen_addrs: Vec<Multiaddr>,
	/// Protocols supported by us.
	pub protocols: Vec<String>,
}

/// Information sent from the listener to the dialer.
#[derive(Debug, Clone)]
pub struct IdentifyInfo {
	/// Public key of the node.
	pub public_key: Vec<u8>,
	/// Version of the "global" protocol, eg. `ipfs/1.0.0` or `polkadot/1.0.0`.
	pub protocol_version: String,
	/// Name and version of the client. Can be thought as similar to the `User-Agent` header
	/// of HTTP.
	pub agent_version: String,
	/// Addresses that the remote is listening on.
	pub listen_addrs: Vec<Multiaddr>,
	/// Our own address as reported by the remote.
	pub observed_addr: Multiaddr,
	/// Protocols supported by the remote.
	pub protocols: Vec<String>,
}

impl<C> ConnectionUpgrade<C> for IdentifyProtocol
    where C: AsyncRead + AsyncWrite + 'static
{
	type NamesIter = iter::Once<(Bytes, Self::UpgradeIdentifier)>;
	type UpgradeIdentifier = ();
	type Output = Option<IdentifyInfo>;
	type Future = Box<Future<Item = Self::Output, Error = IoError>>;

	#[inline]
	fn protocol_names(&self) -> Self::NamesIter {
		iter::once((Bytes::from("/ipfs/id/1.0.0"), ()))
	}

	fn upgrade(self, socket: C, _: (), ty: Endpoint, remote_addr: &Multiaddr) -> Self::Future {
		let socket = socket.framed(VarintCodec::default());

		match ty {
			Endpoint::Dialer => {
				let future = socket.into_future()
				                   .map(|(msg, _)| msg)
				                   .map_err(|(err, _)| err)
				                   .and_then(|msg| if let Some(msg) = msg {
					Ok(Some(parse_proto_msg(msg)?))
				} else {
					Ok(None)
				});

				Box::new(future) as Box<_>
			}

			Endpoint::Listener => {
				let listen_addrs = self.listen_addrs
				                       .into_iter()
				                       .map(|addr| addr.to_string().into_bytes())
				                       .collect();

				let mut message = structs_proto::Identify::new();
				message.set_agentVersion(self.agent_version);
				message.set_protocolVersion(self.protocol_version);
				message.set_publicKey(self.public_key);
				message.set_listenAddrs(listen_addrs);
				message.set_observedAddr(remote_addr.to_string().into_bytes());
				message.set_protocols(RepeatedField::from_vec(self.protocols));

				let bytes = message.write_to_bytes()
					.expect("writing protobuf failed ; should never happen");
				
				// On the server side, after sending the information to the client we make the
				// future produce a `None`. If we were on the client side, this would contain the
				// information received by the server.
				let future = socket.send(bytes).map(|_| None);
				Box::new(future) as Box<_>
			}
		}
	}
}

// Turns a protobuf message into an `IdentifyInfo`. If something bad happens, turn it into
// an `IoError`.
fn parse_proto_msg(msg: BytesMut) -> Result<IdentifyInfo, IoError> {
	match protobuf_parse_from_bytes::<structs_proto::Identify>(&msg) {
		Ok(mut msg) => {
			let listen_addrs = {
				let mut addrs = Vec::new();
				for addr in msg.take_listenAddrs().into_iter() {
					addrs.push(bytes_to_multiaddr(addr)?);
				}
				addrs
			};

			let observed_addr = bytes_to_multiaddr(msg.take_observedAddr())?;

			Ok(IdentifyInfo {
				public_key: msg.take_publicKey(),
				protocol_version: msg.take_protocolVersion(),
				agent_version: msg.take_agentVersion(),
				listen_addrs: listen_addrs,
				observed_addr: observed_addr,
				protocols: msg.take_protocols().into_vec(),
			})
		}

		Err(err) => {
			Err(IoError::new(IoErrorKind::InvalidData, err))
		}
	}
}

// Turn a `Vec<u8>` into a `Multiaddr`. If something bad happens, turn it into an `IoError`.
fn bytes_to_multiaddr(bytes: Vec<u8>) -> Result<Multiaddr, IoError> {
	String::from_utf8(bytes)
		.map_err(|err| {
			IoError::new(IoErrorKind::InvalidData, err)
		})
		.and_then(|s| {
			s.parse()
				.map_err(|err| IoError::new(IoErrorKind::InvalidData, err))
		})
}

#[cfg(test)]
mod tests {
	extern crate libp2p_tcp_transport;
	extern crate tokio_core;

	use self::libp2p_tcp_transport::TcpConfig;
	use self::tokio_core::reactor::Core;
	use IdentifyProtocol;
	use futures::{IntoFuture, Future, Stream};
	use libp2p_swarm::Transport;

	#[test]
	fn basic() {
		let mut core = Core::new().unwrap();
		let tcp = TcpConfig::new(core.handle());
		let with_proto = tcp.with_upgrade(IdentifyProtocol {
			public_key: vec![1, 2, 3, 4],
			protocol_version: "ipfs/1.0.0".to_owned(),
			agent_version: "agent/version".to_owned(),
			listen_addrs: vec!["/ip4/5.6.7.8/tcp/12345".parse().unwrap()],
			protocols: vec!["ping".to_owned(), "kad".to_owned()],
		});

		let (server, addr) = with_proto.clone()
		                  		       .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
		                       		   .unwrap();
		let server = server.into_future()
		                   .map_err(|(err, _)| err)
		                   .and_then(|(n, _)| n.unwrap().0);
		let dialer = with_proto.dial(addr)
		                       .unwrap()
		                       .into_future();

		let (recv, should_be_empty) = core.run(dialer.join(server)).unwrap();
		assert!(should_be_empty.is_none());
		let recv = recv.unwrap();
		assert_eq!(recv.public_key, &[1, 2, 3, 4]);
	}
}
