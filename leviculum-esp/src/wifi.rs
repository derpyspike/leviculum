//! The M9's WiFi: a station in the WLAN `lnflash --wifi` named, and
//! AutoInterface over it, on by default (1479).
//!
//! The station is esp-radio's WiFi driver under the scheduler
//! `radio::start` began; its controller lives in one supervisor future
//! the loop polls once a turn with a no-op waker, exactly as BLE's
//! Columba future is: join, stay joined until the access point goes,
//! wait, join again. Every transition is a `[WIFI] state=` line with the
//! driver's reason, so wrong credentials, an absent access point and one
//! that vanishes mid-session each say what they are, and the board runs
//! on its other carriers meanwhile.
//!
//! There is no IP stack. The driver's station interface hands out raw
//! Ethernet frames, and [`leviculum_core::auto_lan::AutoLan`] is the
//! whole AutoInterface over them: Python's group, ports, token and
//! cadences from `leviculum_core::auto_wire`, a link-local address from
//! the station MAC, the neighbour advertisement a daemon's kernel asks
//! for. A daemon or a Loquax window in the same WLAN sees the board as a
//! neighbour with nothing configured on either side.
//!
//! Memory: the driver allocates only through `malloc_internal`, from the
//! regions `radio::start` and `ble::start` gave esp-alloc; the buffer
//! counts below are cut to what one AutoInterface needs, and the
//! `[WIFI] heap` line after the driver is up says what it took. The
//! AutoInterface state and the frame queues live on the PSRAM heap.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use esp_hal::peripherals::WIFI;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, Interface, Password,
    Ssid, WifiController,
};
use leviculum_core::auto_lan::{AutoLan, Received};
use leviculum_core::auto_wire::{DEFAULT_GROUP_ID, HW_MTU, NONCE_SIZE};
use leviculum_core::traits::{Interface as NodeInterface, InterfaceError};
use leviculum_core::wifi_credentials::WifiCredentials;
use leviculum_core::InterfaceId;

use crate::time_driver::poll_once;
use crate::UartLog;

/// The WiFi interface's index in the node.
pub const WIFI_IFACE: InterfaceId = InterfaceId(2);
/// Its name in the `[NODE]` lines and the interface table: what it is to
/// the mesh, an AutoInterface.
pub const WIFI_NAME: &str = "auto";
/// The neighbours one board keeps on its LAN: a home's daemon, a few
/// windows and phones.
pub const PEERS: usize = 8;
/// The most packets waiting for the station: two seconds of a busy
/// mesh's traffic, the serial queue's bound.
pub const QUEUE_PACKETS: usize = 16;
/// The most received frames one loop turn takes, so a flooded WLAN
/// cannot hold the loop; the rest wait in the driver's queue.
pub const FRAMES_PER_TURN: usize = 16;
/// The first wait before a failed or lost join is tried again; it
/// doubles per failure up to [`RETRY_MAX_MS`] and resets on success.
pub const RETRY_FIRST_MS: u64 = 2_000;
/// The longest wait between joins: an access point that comes back is
/// found within a minute.
pub const RETRY_MAX_MS: u64 = 60_000;

/// The driver's buffers, cut from esp-radio's defaults (10 static and 32
/// dynamic receive, 32 dynamic transmit) to what one AutoInterface on a
/// home WLAN needs: Reticulum packets are at most 1,196 B and arrive a
/// few at a time. No aggregation: it holds buffers for throughput the
/// mesh never asks for.
fn controller_config(station: StationConfig) -> ControllerConfig {
    ControllerConfig::default()
        .with_static_rx_buf_num(4)
        .with_dynamic_rx_buf_num(8)
        .with_dynamic_tx_buf_num(8)
        .with_ampdu_rx_enable(false)
        .with_ampdu_tx_enable(false)
        .with_rx_ba_win(3)
        .with_initial_config(Config::Station(station))
}

/// What the supervisor saw, in order, for the loop to act on and say.
enum News {
    Joined { channel: u8 },
    Refused { reason: Reason, retry_ms: u64 },
    Lost { reason: Reason, retry_ms: u64 },
}

/// A driver reason, printable.
enum Reason {
    Disconnect(esp_radio::wifi::DisconnectReason),
    Driver(esp_radio::wifi::WifiError),
    /// A failure kind a later driver adds.
    Unknown,
}

impl core::fmt::Display for Reason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Disconnect(reason) => write!(f, "{reason:?}"),
            Self::Driver(error) => write!(f, "driver-{error:?}"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// A link change the node must hear of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Joined: the interface is up.
    Up,
    /// Left: the interface is down, its peers gone.
    Down,
}

/// The node's side of the WiFi interface: what the core hands it waits
/// here until the loop gives it to the station. Bounded like the serial
/// queue; offline while the station is not joined, so the core routes
/// around it.
#[derive(Default)]
pub struct WifiIface {
    queue: VecDeque<Vec<u8>>,
    online: bool,
    dropped: u32,
}

impl WifiIface {
    /// An empty queue, offline.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next packet for the station, oldest first.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }

    /// Whether the station is joined.
    pub fn set_online(&mut self, online: bool) {
        self.online = online;
        if !online {
            self.queue.clear();
        }
    }

    /// Packets refused because the queue was full or the link down.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl NodeInterface for WifiIface {
    fn id(&self) -> InterfaceId {
        WIFI_IFACE
    }

    fn name(&self) -> &str {
        WIFI_NAME
    }

    fn mtu(&self) -> usize {
        HW_MTU as usize
    }

    fn is_online(&self) -> bool {
        self.online
    }

    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        if !self.online {
            self.dropped = self.dropped.saturating_add(1);
            return Err(InterfaceError::Disconnected);
        }
        if self.queue.len() >= QUEUE_PACKETS {
            self.dropped = self.dropped.saturating_add(1);
            return Err(InterfaceError::BufferFull);
        }
        self.queue.push_back(data.to_vec());
        Ok(())
    }
}

/// The station and its AutoInterface, as the loop drives them.
pub struct Wifi {
    supervisor: Pin<Box<dyn Future<Output = ()>>>,
    news: Rc<RefCell<VecDeque<News>>>,
    sta: Interface,
    lan: Box<AutoLan<PEERS>>,
    joined: bool,
    /// Frames AutoInterface emitted while a received frame was borrowed.
    pending: Vec<Vec<u8>>,
    /// Frames the driver's transmit queue had no room for.
    tx_dropped: u32,
}

/// Bring the station up for `creds` and start joining. `None`, with the
/// reason on the log, when the driver refused or the credentials cannot
/// be said to it; the board runs on without WiFi. The caller has started
/// the scheduler (`radio::start`).
pub fn start(
    wifi: WIFI<'static>,
    creds: &WifiCredentials,
    nonce: [u8; NONCE_SIZE],
    log: &mut UartLog<'_>,
) -> Option<Wifi> {
    let Ok(ssid) = Ssid::try_from(creds.ssid()) else {
        log.line("[WIFI] ", format_args!("state=off reason=ssid"));
        return None;
    };
    let authentication = if creds.passphrase().is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        match Password::try_from(creds.passphrase()) {
            // WPA2 is the floor the driver asks the access point for;
            // a WPA2/WPA3 network accepts it.
            Ok(password) => AuthenticationMethodConfig::Wpa2Personal(password),
            Err(_) => {
                log.line("[WIFI] ", format_args!("state=off reason=passphrase"));
                return None;
            }
        }
    };
    let station = StationConfig::default()
        .with_ssid(ssid)
        .with_authentication(authentication);
    let controller = match WifiController::new(wifi, controller_config(station)) {
        Ok(controller) => controller,
        Err(e) => {
            log.line("[WIFI] ", format_args!("state=off reason=driver err={e:?}"));
            return None;
        }
    };
    let Some(sta) = Interface::try_station() else {
        log.line("[WIFI] ", format_args!("state=off reason=station-taken"));
        return None;
    };
    log.line(
        "[WIFI] ",
        format_args!(
            "heap used={} free={}",
            esp_alloc::HEAP.used(),
            esp_alloc::HEAP.free()
        ),
    );
    let lan = Box::new(AutoLan::new(sta.mac_address(), DEFAULT_GROUP_ID, nonce));
    log.line(
        "[AUTO] ",
        format_args!(
            "group={} discovery_port={} data_port={} hw_mtu={HW_MTU} peers_max={PEERS}",
            lan.group(),
            leviculum_core::auto_wire::DEFAULT_DISCOVERY_PORT,
            leviculum_core::auto_wire::DEFAULT_DATA_PORT,
        ),
    );
    let news = Rc::new(RefCell::new(VecDeque::new()));
    let supervisor: Pin<Box<dyn Future<Output = ()>>> =
        Box::pin(supervise(controller, Rc::clone(&news)));
    log.line("[WIFI] ", format_args!("state=joining"));
    Some(Wifi {
        supervisor,
        news,
        sta,
        lan,
        joined: false,
        pending: Vec::new(),
        tx_dropped: 0,
    })
}

/// Join, stay joined, wait, join again — for the life of the image.
async fn supervise(mut controller: WifiController<'static>, news: Rc<RefCell<VecDeque<News>>>) {
    let mut retry_ms = RETRY_FIRST_MS;
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                retry_ms = RETRY_FIRST_MS;
                news.borrow_mut().push_back(News::Joined {
                    channel: info.channel,
                });
                let reason = match controller.wait_for_disconnect_async().await {
                    Ok(info) => Reason::Disconnect(info.reason),
                    Err(e) => Reason::Driver(e),
                };
                news.borrow_mut().push_back(News::Lost { reason, retry_ms });
            }
            Err(error) => {
                let reason = match error {
                    ConnectionError::Failed(info) => Reason::Disconnect(info.reason),
                    ConnectionError::WifiError(e) => Reason::Driver(e),
                    _ => Reason::Unknown,
                };
                news.borrow_mut()
                    .push_back(News::Refused { reason, retry_ms });
            }
        }
        embassy_time::Timer::after_millis(retry_ms).await;
        retry_ms = (retry_ms * 2).min(RETRY_MAX_MS);
    }
}

impl Wifi {
    /// One turn: the supervisor polled and its news said, the frames the
    /// driver received taken (data packets into `inbound`), the
    /// AutoInterface timers run, and everything to send handed to the
    /// driver. Returns the link change the node must hear of, if any.
    pub fn turn(
        &mut self,
        now_ms: u64,
        log: &mut UartLog<'_>,
        inbound: &mut Vec<Vec<u8>>,
    ) -> Option<Link> {
        let _ = poll_once(self.supervisor.as_mut());
        let mut change = None;
        loop {
            let Some(news) = self.news.borrow_mut().pop_front() else {
                break;
            };
            match news {
                News::Joined { channel } => {
                    self.joined = true;
                    self.lan.link_up();
                    change = Some(Link::Up);
                    log.line(
                        "[WIFI] ",
                        format_args!("state=up channel={channel} addr={}", self.lan.own()),
                    );
                }
                News::Refused { reason, retry_ms } => {
                    log.line(
                        "[WIFI] ",
                        format_args!("state=refused reason={reason} retry_ms={retry_ms}"),
                    );
                }
                News::Lost { reason, retry_ms } => {
                    self.joined = false;
                    let peers = self.lan.link_down();
                    change = Some(Link::Down);
                    log.line(
                        "[WIFI] ",
                        format_args!(
                            "state=down reason={reason} peers_lost={peers} retry_ms={retry_ms}"
                        ),
                    );
                }
            }
        }
        if !self.joined {
            return change;
        }
        for _ in 0..FRAMES_PER_TURN {
            let Some((rx, _)) = self.sta.receive() else {
                break;
            };
            let lan = &mut self.lan;
            let pending = &mut self.pending;
            let got = rx.consume_token(|frame| {
                match lan.receive(frame, now_ms, &mut |out| pending.push(out.to_vec())) {
                    Received::Data(packet) => Some(Ok(packet.to_vec())),
                    Received::PeerAdded(_) => Some(Err(())),
                    _ => None,
                }
            });
            match got {
                Some(Ok(packet)) => inbound.push(packet),
                Some(Err(())) => log.line(
                    "[AUTO] ",
                    format_args!("peer=added peers={}", self.lan.peer_count()),
                ),
                None => {}
            }
        }
        let pending = &mut self.pending;
        let expired = self.lan.poll(now_ms, &mut |out| pending.push(out.to_vec()));
        if expired > 0 {
            log.line(
                "[AUTO] ",
                format_args!(
                    "peer=expired gone={expired} peers={}",
                    self.lan.peer_count()
                ),
            );
        }
        self.flush();
        change
    }

    /// One packet the node sent, to every AutoInterface peer.
    pub fn send(&mut self, packet: &[u8]) {
        if !self.joined {
            return;
        }
        let pending = &mut self.pending;
        self.lan.send(packet, &mut |out| pending.push(out.to_vec()));
        self.flush();
    }

    /// Frames refused by the driver's full transmit queue, for the
    /// census line.
    pub fn tx_dropped(&self) -> u32 {
        self.tx_dropped
    }

    fn flush(&mut self) {
        for frame in self.pending.drain(..) {
            match self.sta.transmit() {
                Some(tx) => tx.consume_token(frame.len(), |buf| buf.copy_from_slice(&frame)),
                None => self.tx_dropped = self.tx_dropped.saturating_add(1),
            }
        }
    }
}
