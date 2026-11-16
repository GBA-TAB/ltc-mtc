//! serial-midi: a MIDI port on a serial line, for MIDI boards on the Pi's UART (GPIO 14/15).
//!
//! The program is the sequencer client "Serial MIDI" (`--name`) with one port "MIDI":
//! - events sent to the port (ltc-mtc's output, `aconnect`) go out on the serial line;
//! - bytes coming in on the serial line are sent to the port's subscribers.
//!
//! The UART runs at 31.25 kbaud with `dtoverlay=midi-uart0` in config.txt, which makes a
//! requested 38400 baud come out as 31250. That is why the default speed is 38400.

use alsa::poll::Descriptors;
use alsa::seq::{MidiEvent, PortCap, PortType, Seq};
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

fn usage() -> ! {
    eprintln!("usage: serial-midi [--device /dev/ttyAMA0] [--baud 38400] [--name \"Serial MIDI\"]");
    std::process::exit(2)
}

fn speed(baud: u32) -> libc::speed_t {
    match baud {
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        _ => {
            eprintln!("unsupported baud rate {baud}");
            std::process::exit(2)
        }
    }
}

/// Opens the serial line raw, 8N1, no flow control, reads returning what is there.
fn open_serial(path: &str, baud: u32) -> std::io::Result<OwnedFd> {
    let fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)?;
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd.as_raw_fd(), &mut t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag &= !(libc::CSTOPB | libc::CRTSCTS);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::cfsetispeed(&mut t, speed(baud));
        libc::cfsetospeed(&mut t, speed(baud));
        if libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::tcflush(fd.as_raw_fd(), libc::TCIOFLUSH);
    }
    Ok(fd.into())
}

fn write_all(fd: &OwnedFd, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// Real-time scheduling for this thread, if allowed (systemd unit or rtprio limit).
fn set_realtime(prio: i32) {
    unsafe {
        let p = libc::sched_param { sched_priority: prio };
        if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &p) != 0 {
            eprintln!("note: no real-time priority (run as the systemd unit, or allow rtprio)");
        }
    }
}

fn main() {
    let mut device = "/dev/ttyAMA0".to_string();
    let mut baud = 38400;
    let mut name = "Serial MIDI".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--device" => device = val(),
            "--baud" => baud = val().parse().unwrap_or_else(|_| usage()),
            "--name" => name = val(),
            _ => usage(),
        }
    }

    let serial = open_serial(&device, baud).unwrap_or_else(|e| {
        eprintln!("{device}: {e}");
        std::process::exit(1)
    });
    let seq = Seq::open(None, None, true).unwrap_or_else(|e| {
        eprintln!("ALSA sequencer: {e}");
        std::process::exit(1)
    });
    seq.set_client_name(&CString::new(name.as_str()).unwrap()).unwrap();
    let caps = PortCap::READ | PortCap::SUBS_READ | PortCap::WRITE | PortCap::SUBS_WRITE;
    let kind = PortType::MIDI_GENERIC | PortType::HARDWARE | PortType::PORT;
    let port = seq.create_simple_port(&CString::new("MIDI").unwrap(), caps, kind).unwrap();
    let client = seq.client_id().unwrap();
    eprintln!("{device} at {baud} baud <-> sequencer port {client}:{port} \"{name}:MIDI\"");
    set_realtime(70);

    // Sequencer -> bytes: full status bytes (no running status), so a receiver that missed a
    // byte or started late picks up at the next message.
    let to_bytes = MidiEvent::new(256).unwrap();
    to_bytes.enable_running_status(false);
    // Bytes -> sequencer. A SysEx longer than the buffer goes out in pieces.
    let mut to_events = MidiEvent::new(256).unwrap();

    let mut input = seq.input();
    let mut fds = (&seq, Some(alsa::Direction::Capture)).get().unwrap();
    fds.push(libc::pollfd { fd: serial.as_raw_fd(), events: libc::POLLIN, revents: 0 });
    let serial_idx = fds.len() - 1;
    let mut buf = [0u8; 256];
    loop {
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("poll: {e}");
            std::process::exit(1);
        }

        // Sequencer events -> serial line.
        loop {
            let mut ev = match input.event_input() {
                Ok(ev) => ev,
                Err(e) if e.errno() == libc::EAGAIN => break,
                Err(e) if e.errno() == libc::ENOSPC => {
                    eprintln!("sequencer input overrun: events lost");
                    continue;
                }
                Err(e) => {
                    eprintln!("sequencer input: {e}");
                    std::process::exit(1)
                }
            };
            // Non-MIDI events (subscriptions, announcements) do not decode: skip them.
            if let Ok(n) = to_bytes.decode(&mut buf, &mut ev) {
                if let Err(e) = write_all(&serial, &buf[..n]) {
                    eprintln!("{device}: {e}");
                    std::process::exit(1)
                }
            }
        }

        // Serial line -> sequencer.
        if fds[serial_idx].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            eprintln!("{device}: line closed");
            std::process::exit(1)
        }
        if fds[serial_idx].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(serial.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                eprintln!("{device}: {}", std::io::Error::last_os_error());
                std::process::exit(1)
            }
            let mut rest = &buf[..n as usize];
            while !rest.is_empty() {
                let (used, ev) = match to_events.encode(rest) {
                    Ok(r) => r,
                    Err(_) => {
                        to_events.reset_encode();
                        break;
                    }
                };
                if let Some(mut ev) = ev {
                    ev.set_source(port);
                    ev.set_subs();
                    ev.set_direct();
                    let _ = seq.event_output_direct(&mut ev);
                }
                if used == 0 {
                    break;
                }
                rest = &rest[used..];
            }
        }
    }
}
