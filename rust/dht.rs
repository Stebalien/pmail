extern crate time;

use std::net::{SocketAddr, SocketAddrV6, SocketAddrV4,
               Ipv6Addr, Ipv4Addr};
use onionsalt;
use onionsalt::{ROUTING_LENGTH,
                PAYLOAD_LENGTH};
use onionsalt::{crypto,
                onionbox,
                onionbox_open};
use std::io::Error;
use std;
use super::udp;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc,Mutex};
use std::sync::mpsc::{Sender};
use std::thread;

trait MyBytes<T> {
    fn bytes(&self, &mut T);
    fn from_bytes(&T) -> Self;
}

impl MyBytes<[u8; 18]> for SocketAddr {
    fn bytes(&self, out: &mut[u8; 18]) {
        match *self {
            SocketAddr::V6(sa) => {
                // is it an IPv4-mapped IPv6 address?
                match sa.ip().to_ipv4() {
                    None => (),
                    Some(ipv4) => {
                        for i in 0..18 {
                            out[i] = 0;
                        }
                        sa.port().bytes(array_mut_ref![out, 2, 2]);
                        *array_mut_ref![out,4,4] = ipv4.octets();
                        return;
                    },
                }
                let (port, a, b, c, d, e, f, g, h) = mut_array_refs!(out, 2,
                                                                     2,2,2,2,
                                                                     2,2,2,2);
                sa.port().bytes(port);
                let addr = sa.ip().segments();
                addr[0].bytes(a); addr[1].bytes(b); addr[2].bytes(c); addr[3].bytes(d);
                addr[4].bytes(e); addr[5].bytes(f); addr[6].bytes(g); addr[7].bytes(h);
            },
            SocketAddr::V4(sa) => {
                for i in 0..18 {
                    out[i] = 0;
                }
                sa.port().bytes(array_mut_ref![out, 2, 2]);
                *array_mut_ref![out,4,4] = sa.ip().octets();
            },
        }
    }
    fn from_bytes(inp: &[u8; 18]) -> SocketAddr {
        if inp[0] == 0 && inp[1] == 0 {
            SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(inp[4], inp[5], inp[6], inp[7]),
                u16::from_bytes(array_ref![inp,2,2])))
        } else {
            let mut addr = [0; 8];
            for i in 0..8 {
                addr[i] = u16::from_bytes(array_ref![inp,2 + 2*i, 2]);
            }
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(addr[0],addr[1],addr[2],addr[3],
                              addr[4],addr[5],addr[6],addr[7]),
                u16::from_bytes(array_ref![inp,0,2]), 0, 0))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoutingInfo {
    /// The IP addressing information, 18 bytes, enough for an ipv6
    /// address and a 16 bit port.
    ip: SocketAddr,
    /// The time in seconds (after some specified epoch) by which we
    /// want the message to arrive.  4 bytes/
    eta: u32,
    /// The payload is for us!  Wheee!
    is_for_me: bool,
    /// Just respond to the sender of this packet with his address.
    /// This is needed for a node to find out its port, if it happens
    /// to be passing through a NAT.
    who_am_i: bool,
}

impl MyBytes<[u8; 4]> for u32 {
    fn bytes(&self, out: &mut[u8; 4]) {
        out[0] = *self as u8;
        out[1] = (*self >> 8) as u8;
        out[2] = (*self >> 16) as u8;
        out[3] = (*self >> 24) as u8;
    }
    fn from_bytes(inp: &[u8; 4]) -> u32 {
        inp[0] as u32 + ((inp[1] as u32) << 8) + ((inp[2] as u32) << 16) + ((inp[3] as u32) << 24)
    }
}

impl MyBytes<[u8; 2]> for u16 {
    fn bytes(&self, out: &mut[u8; 2]) {
        out[0] = *self as u8;
        out[1] = (*self >> 8) as u8;
    }
    fn from_bytes(inp: &[u8; 2]) -> u16 {
        inp[0] as u16 + ((inp[1] as u16) << 8)
    }
}

impl MyBytes<[u8; ROUTING_LENGTH]> for RoutingInfo {
    fn bytes(&self, out: &mut[u8; ROUTING_LENGTH]) {
        let (flags,addr,eta,_) = mut_array_refs!(out, 1, 18, 4,1);
        flags[0] = self.is_for_me as u8 + ((self.who_am_i as u8) << 1);
        self.ip.bytes(addr);
        self.eta.bytes(eta);
    }
    fn from_bytes(inp: &[u8; ROUTING_LENGTH]) -> RoutingInfo {
        let (flags,addr,eta,_) = array_refs!(inp, 1, 18, 4,1);
        RoutingInfo {
            ip: SocketAddr::from_bytes(addr),
            eta: u32::from_bytes(eta),
            is_for_me: flags[0] & 1 == 1,
            who_am_i: flags[0] & 2 == 2,
        }
    }
}

/// When we ask for addresses, we only get `NUM_IN_RESPONSE`, since
/// that is all that will fit in the payload, along with their public
/// key and the authentication overhead.
pub const NUM_IN_RESPONSE: usize = 10;


/// The `USER_MESSAGE_LENGTH` is the size of actual content that can
/// be encrypted and authenticated to send to some receiver.
pub const USER_MESSAGE_LENGTH: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoutingGift {
    pub addr: SocketAddr,
    pub key: crypto::PublicKey,
}

impl MyBytes<[u8; 32]> for crypto::PublicKey {
    fn bytes(&self, out: &mut[u8; 32]) {
        *out = self.0;
    }
    fn from_bytes(inp: &[u8; 32]) -> crypto::PublicKey {
        crypto::PublicKey(*inp)
    }
}

impl MyBytes<[u8; 18+32]> for RoutingGift {
    fn bytes(&self, out: &mut[u8; 18+32]) {
        self.addr.bytes(array_mut_ref![out,0,18]);
        self.key.bytes(array_mut_ref![out,18,32]);
    }
    fn from_bytes(inp: &[u8; 18+32]) -> RoutingGift {
        RoutingGift {
            addr: SocketAddr::from_bytes(array_ref![inp,0,18]),
            key: crypto::PublicKey::from_bytes(array_ref![inp,18,32]),
        }
    }
}

type RoutingGifts = [RoutingGift; NUM_IN_RESPONSE];

impl MyBytes<[u8; (18+32)*NUM_IN_RESPONSE]> for [RoutingGift; NUM_IN_RESPONSE] {
    fn bytes(&self, out: &mut[u8; (18+32)*NUM_IN_RESPONSE]) {
        for i in 0 .. NUM_IN_RESPONSE {
            self[i].bytes(array_mut_ref![out,i*(18+32),18+32]);
        }
    }
    fn from_bytes(inp: &[u8; (18+32)*NUM_IN_RESPONSE]) -> [RoutingGift; NUM_IN_RESPONSE] {
        let (g0,g1,g2,g3,g4,g5,g6,g7,g8,g9) = array_refs!(inp,
                                                          50, 50, 50, 50, 50,
                                                          50, 50, 50, 50, 50);
        [RoutingGift::from_bytes(g0), RoutingGift::from_bytes(g1),
         RoutingGift::from_bytes(g2), RoutingGift::from_bytes(g3),
         RoutingGift::from_bytes(g4), RoutingGift::from_bytes(g5),
         RoutingGift::from_bytes(g6), RoutingGift::from_bytes(g7),
         RoutingGift::from_bytes(g8), RoutingGift::from_bytes(g9)]
    }
}


// impl RoutingGift {
//     fn new() -> RoutingGift {
//         RoutingGift {
//             addr: Addr::V4{ addr: [0;4], port: 0 },
//             key: crypto::PublicKey([0;32]),
//         }
//     }
// }

pub enum Message {
    Greetings([RoutingGift; NUM_IN_RESPONSE]),
    Response([RoutingGift; NUM_IN_RESPONSE]),
    PickUp {
        destination: crypto::PublicKey,
        gifts: [RoutingGift; NUM_IN_RESPONSE],
    },
    ForwardPlease {
        destination: crypto::PublicKey,
        message: [u8; 512], // this is a hokey and lazy
                                 // workaround for lack of derive for
                                 // arrays larger than 32.
    },
}

impl MyBytes<[u8; PAYLOAD_LENGTH]> for Message {
    fn bytes(&self, out: &mut[u8; PAYLOAD_LENGTH]) {
        match *self {
            Message::Greetings(gifts) => {
                out[0] = b'g';
                gifts.bytes(array_mut_ref![out,1,500]);
            },
            Message::Response(gifts) => {
                out[0] = b'r';
                gifts.bytes(array_mut_ref![out,1,500]);
            },
            Message::PickUp { destination, gifts } => {
                out[0] = b'p';
                destination.bytes(array_mut_ref![out,1,32]);
                gifts.bytes(array_mut_ref![out,33,500]);
            },
            Message::ForwardPlease { destination, message } => {
                out[0] = b'f';
                destination.bytes(array_mut_ref![out,1,32]);
                *array_mut_ref![out,33,512] = message;
            },
        }
    }
    fn from_bytes(inp: &[u8; PAYLOAD_LENGTH]) -> Message {
        match inp[0] {
            b'g' => Message::Greetings(RoutingGifts::from_bytes(array_ref![inp,1,500])),
            b'r' => Message::Response(RoutingGifts::from_bytes(array_ref![inp,1,500])),
            b'p' => unimplemented!(),
            b'f' => unimplemented!(),
            _ => unimplemented!(),
        }
    }
}


// impl Message {
//     fn encrypted_bytes(&self,
//                        n: crypto::Nonce,
//                        to: crypto::PublicKey,
//                        from: crypto::KeyPair) -> [u8; PAYLOAD_LENGTH] {
//         let mut plain = [0; PAYLOAD_LENGTH - 16];
//         // FIXME fill up plain here with the actual content to send.
//         let mut out = [0; PAYLOAD_LENGTH];
//         crypto::box_up(array_mut_ref![out, 16, u8, PAYLOAD_LENGTH-16],
//                        &plain, &n, &to, from.secret).unwrap();
//         *array_mut_ref![out, 0, u8, 32] = from.public.0;
//         out
//     }
//     fn encrypt(&self,
//                to: crypto::PublicKey,
//                from: crypto::KeyPair,
//                addresses: HashMap<crypto::PublicKey, Addr>) -> RawEncryptedMessage {
//         match self.contents {
//             Greetings(gift) => {
//                 let pk = crypto::box_keypair();
//                 let payload = self.encrypted_bytes(crypto::Nonce(pk.public.0), to, from);
//                 let ob = onionsalt::onionbox(&[(to, RoutingInfo::empty(20).bytes())],
//                                              &payload, 0, &pk).unwrap();
//                 ob.packet();
//             },
//             _ => {
//                 unimplemented!();
//             },
//         }
//     }
// }

/// The `EPOCH` is when time begins.  We have not facilities for
/// sending messages prior to this time.  However, we also do not
/// promise never to change `EPOCH`.  It may be changed in a future
/// version of the protocol in order to avoid a Y2K-like problem.
/// Therefore, `EPOCH`-difference times should not be stored on disk,
/// and should only be sent over the network.  It is also possible
/// that future changes will needlessly change `EPOCH` (but only while
/// making other network protool changes) simply to flush out
/// buggy use of thereof.
const EPOCH: time::Timespec = time::Timespec { sec: 1420092000, nsec: 0 };

pub fn now() -> u32 {
    (time::get_time().sec - EPOCH.sec) as u32
}

impl RoutingInfo {
    pub fn new(saddr: SocketAddr, delay_time: u32) -> RoutingInfo {
        let eta = now() + delay_time;
        RoutingInfo {
            ip: saddr,
            eta: eta,
            is_for_me: false,
            who_am_i: false,
        }
    }
}

pub fn read_keypair(name: &std::path::Path) -> Result<crypto::KeyPair, Error> {
    use std::io::Read;

    let mut f = try!(std::fs::File::open(name));
    let mut data = Vec::new();
    try!(f.read_to_end(&mut data));
    if data.len() != 64 {
        return Err(Error::new(std::io::ErrorKind::Other, "oh no!"));
    }
    Ok(crypto::KeyPair {
        public: crypto::PublicKey(*array_ref![data, 0, 32]),
        secret: crypto::SecretKey(*array_ref![data, 32, 32]),
    })
}

/// This is just a crude guess as to the hostname.  I didn't put much
/// effort into this because it is only really relevant in the case
/// where you have a shared home directory for multiple different
/// computers that should be independently running pmail.
pub fn gethostname() -> Result<String, Error> {
    use std::io::Read;

    let mut f = try!(std::fs::File::open("/etc/hostname"));
    let mut hostname = String::new();
    try!(f.read_to_string(&mut hostname));
    match hostname.split_whitespace().next() {
        Some(hn) => Ok(String::from(hn)),
        None => Err(Error::new(std::io::ErrorKind::Other, "malformed /etc/hostname")),
    }
}

pub fn read_or_generate_keypair(mut dirname: std::path::PathBuf)
                                -> Result<crypto::KeyPair, Error> {
    use std::io::Write;

    match gethostname() {
        Err(_) => {
            dirname.push(".pmail.key");
        },
        Ok(hostname) => {
            dirname.push(format!(".pmail-{}.key", hostname));
        },
    };
    let name = dirname.as_path();
    match read_keypair(name) {
        Ok(kp) => Ok(kp),
        _ => {
            match crypto::box_keypair() {
                Err(_) => Err(Error::new(std::io::ErrorKind::Other, "oh bad random!")),
                Ok(kp) => {
                    let mut f = try!(std::fs::File::create(name));
                    let mut data = [0; 64];
                    *array_mut_ref![data, 0, 32] = kp.public.0;
                    *array_mut_ref![data, 32, 32] = kp.secret.0;
                    try!(f.write_all(&data));
                    print!("Created new key!  [");
                    for i in 0..32 {
                        print!("{}, ", kp.public.0[i]);
                    }
                    println!("]");
                    Ok(kp)
                }
            }
        }
    }
}

fn bingley() -> RoutingGift {
    let bingley_addr = SocketAddr::from_str("128.193.96.51:54321").unwrap();
    let bingley_key = crypto::PublicKey([212, 73, 217, 51, 40, 221, 144,
                                         145, 86, 176, 174, 255, 41, 29,
                                         172, 191, 136, 196, 210, 157, 215,
                                         11, 144, 238, 198, 47, 200, 43,
                                         227, 172, 76, 45]);
    RoutingGift { addr: bingley_addr, key: bingley_key }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DHT {
    // has_been_seen: HashMap<crypto::PublicKey, u32>,
    // never_seen: HashSet<crypto::PublicKey>,
    addresses: HashMap<crypto::PublicKey, SocketAddr>,
    pubkeys: HashMap<SocketAddr, crypto::PublicKey>,
    my_key: crypto::KeyPair,
    /// When we send messages, we should store their OnionBoxen in this
    /// map, so we can listen for the return...
    onionboxen: HashMap<[u8; 32], onionsalt::OnionBox>,
}

fn codename(text: &[u8]) -> String {
    let adjectives = ["good", "happy", "nice", "evil", "sloppy", "slovenly",
                      "meticulous", "beloved", "hateful", "green", "lovely",
                      "corporate", "presidential", "stately", "serene",
                      "indignant", "exciting", "one", "fluffy",
                      "sour", "hot", "sexy", "absent minded", "considerate"];
    let nouns = ["warthog", "vampire", "person", "nemesis", "pooch",
                 "superhero", "scientist", "writer", "author", "oboist",
                 "physicist", "musicologist", "teacher", "professor",
                 "squirrel", "deer", "beaver", "duck", "poodle",
                 "republican", "democrat",
                 "bunny", "cat", "kitty", "boy", "girl", "man", "woman"];
    if text.len() < 2 {
        return format!("{:?}", text);
    }
    format!("{} {}", adjectives[text[0] as usize % adjectives.len()],
            nouns[text[1] as usize % nouns.len()])
}

impl DHT {
    fn new(myself: &crypto::KeyPair) -> Arc<Mutex<DHT>> {
        let mut dht = DHT {
            addresses: HashMap::new(),
            pubkeys: HashMap::new(),
            onionboxen: HashMap::new(),
            my_key: *myself,
        };
        // initialize a the mappings!
        dht.accept_single_gift(&bingley());
        Arc::new(Mutex::new(dht))
    }
    fn construct_gift(&mut self) -> [RoutingGift; NUM_IN_RESPONSE] {
        let mut out = [bingley(); NUM_IN_RESPONSE];
        for i in 0..NUM_IN_RESPONSE {
            out[i] = self.random_gift();
        }
        out
    }
    fn accept_single_gift(&mut self, g: &RoutingGift) {
        self.addresses.insert(g.key, g.addr);
        self.pubkeys.insert(g.addr, g.key);
    }
    fn accept_gift(&mut self, gift: &[RoutingGift; NUM_IN_RESPONSE]) {
        for g in gift {
            self.accept_single_gift(g);
        }
    }
    fn random_key(&mut self) -> crypto::PublicKey {
        let i = self.random_usize() % self.addresses.len();
        let mut keys = self.addresses.keys();
        *keys.nth(i).unwrap()
    }
    fn random_gift(&mut self) -> RoutingGift {
        let k = self.random_key();
        RoutingGift { key: k, addr: self.addresses[&k] }
    }
    fn random_usize(&mut self) -> usize {
        // The following is a really stupid way of getting a random
        // usize so that we will send to a random node each time.
        let r = crypto::random_nonce().unwrap().0;
        r[0] as usize + ((r[1] as usize)<<8) + ((r[2] as usize)<<16)
    }
    fn pick_route(&mut self) -> Vec<RoutingGift> {
        let mut out = Vec::new();
        let mut old_gift = RoutingGift {
            key: self.my_key.public,
            addr: self.addresses[&self.my_key.public],
        };
        for _ in 0 .. 3 + (self.random_usize() % 4) {
            let mut new_gift = self.random_gift();
            while new_gift.key == old_gift.key {
                new_gift = self.random_gift();
            }
            if new_gift.key == self.my_key.public {
                // There is no point creating a loop that passes
                // *through* myself.
                return out;
            }
            out.push(new_gift);
            old_gift = new_gift;
        }
        out
    }
    fn greet(&mut self) -> (SocketAddr, onionsalt::OnionBox) {
        let mut payload = [0; PAYLOAD_LENGTH];
        Message::Greetings(self.construct_gift()).bytes(&mut payload);

        let route = self.pick_route();
        let mut recipient = self.random_usize() % route.len();
        // avoid sending greetings to myself!
        while route[recipient].key == self.my_key.public {
            recipient = self.random_usize() % route.len();
        }
        println!("\nSending a nice greeting loop of length {}", route.len());
        let mut keys_and_routes = Vec::new();
        let mut delay_time = 0;
        for i in 0 .. route.len() {
            let mut k_and_r = (route[i].key, [0; ROUTING_LENGTH]);
            let next_addr = if i < route.len()-1 {
                route[i+1].addr
            } else {
                self.addresses[&self.my_key.public]
            };
            if i == recipient {
                println!(" => {}", route[i].addr);
            } else {
                println!("    {}", route[i].addr);
            }
            delay_time += 10 + self.random_usize() as u32 % 60;
            let mut ri = RoutingInfo::new(next_addr, delay_time);
            ri.is_for_me = i == recipient;
            ri.who_am_i = false;
            ri.bytes(&mut k_and_r.1);
            keys_and_routes.push(k_and_r);
        }

        let mut ob = onionbox(&keys_and_routes, recipient).unwrap();
        ob.add_payload(self.my_key, &payload);
        println!("greeting code name: {}\n", codename(&ob.return_magic()));
        (route[0].addr, ob)
    }
    fn whoami(&mut self, who: &RoutingGift) -> (SocketAddr, onionsalt::OnionBox) {
        println!("Sending a whoami to {}", who.addr);
        let mut hello_payload = [0; PAYLOAD_LENGTH];
        Message::Greetings([*who; NUM_IN_RESPONSE]).bytes(&mut hello_payload);

        let mut keys_and_routes = [(who.key, [0; ROUTING_LENGTH])];
        let mut ri = RoutingInfo::new(who.addr, 60);
        ri.is_for_me = true;
        ri.who_am_i = true;
        ri.bytes(&mut keys_and_routes[0].1);

        let mut ob = onionbox(&keys_and_routes, 0).unwrap();
        ob.add_payload(self.my_key, &hello_payload);
        println!("whoami code name: {}\n", codename(&ob.return_magic()));
        (who.addr, ob)
    }

    fn maintenance(&mut self) -> (SocketAddr, onionsalt::OnionBox) {
        if !self.addresses.contains_key(&self.my_key.public) || self.addresses.len() < 2 || self.random_usize() % 10 == 0 {
            println!("Routing table:");
            for (k,a) in self.addresses.iter() {
                println!(" {} -> {}", k, a);
            }
            let gift = self.random_gift();
            return self.whoami(&gift);
        }
        self.greet()
    }
}

/// Start relaying messages with a static public key (i.e. one that
/// does not change).
pub fn start_static_node() -> Result<(), Error> {
    let keydirname = match std::env::home_dir() {
        Some(hd) => hd,
        None => std::path::PathBuf::from("."),
    };
    let my_key = read_or_generate_keypair(keydirname).unwrap();

    let dht = DHT::new(&my_key);

    let (lopriority, send, get, _) = try!(udp::listen());

    {
        // Here we set up the thread that sends out requests for
        // routing information.  This thread should wake up no more
        // than once every 10 seconds (until I increase the
        // communication frequency), and should ensure that we're
        // always ready to send *something* out.
        let dht = dht.clone(); // a separate copy for sending
                               // maintenance requests.
        std::thread::spawn(move|| {
            loop {
                let (addr,ob) = dht.lock().unwrap().maintenance();
                let msg = udp::RawEncryptedMessage{
                    ip: addr,
                    data: ob.packet(),
                };
                // The following enables us to easily check for a response
                // to this message.
                dht.lock().unwrap().onionboxen.insert(ob.return_magic(), ob);
                lopriority.send(msg).unwrap();
            }
        });
    }

    for packet in get.iter() {
        match onionbox_open(&packet.data, &my_key.secret) {
            Ok(mut oob) => {
                let routing = RoutingInfo::from_bytes(&oob.routing());
                if routing.is_for_me {
                    match oob.payload(&my_key) {
                        Err(e) => {
                            println!("Unable to read message! {:?}", e);
                        },
                        Ok(payload) => {
                            if routing.who_am_i {
                                println!("Got whoami from {}", packet.ip);
                                let mut you_are = [0; PAYLOAD_LENGTH];
                                let mut gift = dht.lock().unwrap().construct_gift();
                                gift[0] = RoutingGift{ addr: packet.ip,
                                                       key: oob.key() };
                                // add the sender to our database of routers
                                dht.lock().unwrap().accept_single_gift(&gift[0]);
                                Message::Response(gift).bytes(&mut you_are);
                                oob.respond(&my_key, &you_are);
                                send_delayed(&send, routing.eta, &udp::RawEncryptedMessage{
                                    ip: packet.ip,
                                    data: oob.packet(),
                                });
                            } else {
                                match Message::from_bytes(&payload) {
                                    Message::Greetings(gs) => {
                                        dht.lock().unwrap().accept_gift(&gs);
                                        println!("Got greeting from {}", packet.ip);

                                        let mut response = [0; PAYLOAD_LENGTH];
                                        let gift = dht.lock().unwrap().construct_gift();
                                        Message::Response(gift).bytes(&mut response);
                                        oob.respond(&my_key, &response);
                                        send_delayed(&send, routing.eta, &udp::RawEncryptedMessage{
                                            ip: routing.ip,
                                            data: oob.packet(),
                                        });
                                    },
                                    _ => {
                                        println!("Something else for me!\n\n");
                                    },
                                }
                            }
                        }
                    }
                } else {
                    // This is a packet that we should relay along.
                    println!("I am relaying packet {} -> {}",
                             packet.ip, routing.ip);
                    send_delayed(&send, routing.eta, &udp::RawEncryptedMessage{
                        ip: routing.ip,
                        data: oob.packet(),
                    });
                }
            },
            _ => {
                let maybe_msg = match dht.lock().unwrap().onionboxen.get(array_ref![packet.data,0,32]) {
                    Some(ob) =>
                        match ob.read_return(my_key, &packet.data) {
                            Ok(msg) => {
                                println!("Response to code name {}!",
                                         codename(array_ref![packet.data,0,32]));
                                Some(msg)
                            },
                            _ => {
                                println!("Message illegible!");
                                None
                            },
                        },
                    None => {
                        println!("Not sure what that was! (from {})", packet.ip);
                        None
                    },
                };
                match maybe_msg.map(|x| { Message::from_bytes(&x) }) {
                    None => (),
                    Some(Message::Greetings(_)) => {
                        println!("Greetings not a valid response");
                    },
                    Some(Message::Response(rgs)) => {
                        dht.lock().unwrap().accept_gift(&rgs);
                        if rgs[0].key == my_key.public {
                            println!("My address is {}", rgs[0].addr);
                        }
                    },
                    Some(Message::PickUp {..}) => {
                        println!("Pickup not yet handled");
                    },
                    Some(Message::ForwardPlease {..}) => {
                        println!("Forward not yet handled");
                    },
                }
                if maybe_msg.is_some() {
                    dht.lock().unwrap().onionboxen.remove(array_ref![packet.data,0,32]);
                }
            },
        }
    }
    // lopriority.send();
    Ok(())
}

fn send_delayed(send: &Sender<udp::RawEncryptedMessage>,
                eta: u32, msg: &udp::RawEncryptedMessage) {
    let n = now();
    let delay = if eta < n { 0 } else { eta - n };
    if delay == 0 {
        println!("Send is late by {} seconds!", n - eta);
        send.send(*msg).unwrap();
    } else {
        println!("Delaying send by {} seconds", delay);
        let msg = *msg;
        let send = send.clone();
        thread::spawn(move|| {
            thread::sleep_ms(delay*1000);
            send.send(msg).unwrap();
        });
    }
}
