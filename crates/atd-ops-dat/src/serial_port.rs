//! Opening a serial port at a baud rate, 8 data bits, no parity, one stop
//! bit, raw (no line editing), with reads that return after at most about
//! 100 ms (zero bytes when nothing arrived). POSIX termios on macOS and
//! Linux, the Win32 comm API on Windows.

use std::io;

/// A port as the worker uses it.
pub trait Port: io::Read + io::Write + Send {}
impl<T: io::Read + io::Write + Send> Port for T {}

/// Opens `name` (`/dev/tty.usbmodem1101`, `/dev/ttyUSB0`, `COM3`).
pub fn open(name: &str, baud: u32) -> io::Result<Box<dyn Port>> {
    imp::open(name, baud)
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use super::Port;

    #[cfg(target_os = "macos")]
    fn speed(baud: u32) -> io::Result<libc::speed_t> {
        // macOS takes the rate itself.
        Ok(libc::speed_t::from(baud))
    }

    #[cfg(not(target_os = "macos"))]
    fn speed(baud: u32) -> io::Result<libc::speed_t> {
        Ok(match baud {
            300 => libc::B300,
            600 => libc::B600,
            1200 => libc::B1200,
            2400 => libc::B2400,
            4800 => libc::B4800,
            9600 => libc::B9600,
            19200 => libc::B19200,
            38400 => libc::B38400,
            57600 => libc::B57600,
            115_200 => libc::B115200,
            230_400 => libc::B230400,
            #[cfg(target_os = "linux")]
            460_800 => libc::B460800,
            #[cfg(target_os = "linux")]
            921_600 => libc::B921600,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{baud} baud is not supported here"),
                ));
            }
        })
    }

    pub fn open(name: &str, baud: u32) -> io::Result<Box<dyn Port>> {
        // Non-blocking to open (a modem line can wait for carrier), then
        // blocking reads bounded by VTIME.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(name)?;
        let fd = file.as_raw_fd();
        let speed = speed(baud)?;
        // SAFETY: `fd` is an open descriptor owned by `file` for the whole
        // block; `t` is a plain C struct, filled by tcgetattr before use.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::cfmakeraw(&mut t);
            t.c_cflag |= libc::CLOCAL | libc::CREAD;
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 1;
            if libc::cfsetispeed(&mut t, speed) != 0 || libc::cfsetospeed(&mut t, speed) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Box::new(file))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Devices::Communication::{
        COMMTIMEOUTS, DCB, GetCommState, SetCommState, SetCommTimeouts,
    };

    use super::Port;

    pub fn open(name: &str, baud: u32) -> io::Result<Box<dyn Port>> {
        // COM10 and up need the device namespace; it works for all.
        let path = if name.starts_with(r"\\.\") {
            name.to_owned()
        } else {
            format!(r"\\.\{name}")
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
        let h = file.as_raw_handle();
        // SAFETY: `h` is the open handle owned by `file` for the whole
        // block; DCB and COMMTIMEOUTS are plain C structs.
        unsafe {
            let mut dcb: DCB = std::mem::zeroed();
            dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
            if GetCommState(h, &mut dcb) == 0 {
                return Err(io::Error::last_os_error());
            }
            dcb.BaudRate = baud;
            // fBinary on, every flow control and parity check off.
            dcb._bitfield = 1;
            dcb.ByteSize = 8;
            dcb.Parity = 0;
            dcb.StopBits = 0;
            if SetCommState(h, &dcb) == 0 {
                return Err(io::Error::last_os_error());
            }
            // Return at once with what is there, else wait up to 100 ms.
            let timeouts = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: u32::MAX,
                ReadTotalTimeoutConstant: 100,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: 1000,
            };
            if SetCommTimeouts(h, &timeouts) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Box::new(file))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Port;

    pub fn open(_name: &str, _baud: u32) -> std::io::Result<Box<dyn Port>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "serial ports are not supported on this platform",
        ))
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    /// The termios path on a pseudo-terminal (no serial hardware needed):
    /// raw mode, the baud rate set, reads bounded by the timeout.
    #[test]
    fn a_pseudo_terminal_opens_raw_with_bounded_reads() {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: openpty writes two descriptors; name, termios and window
        // size are optional (null).
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: `slave` is open; ttyname returns a static C string.
        let name = unsafe { std::ffi::CStr::from_ptr(libc::ttyname(slave)) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: `master` is an open descriptor this test now owns.
        let mut master = unsafe { std::fs::File::from_raw_fd(master) };
        let mut port = super::open(&name, 115_200).unwrap();
        // SAFETY: the extra slave descriptor from openpty is ours to close.
        unsafe { libc::close(slave) };
        let mut buf = [0u8; 16];
        let t = std::time::Instant::now();
        assert_eq!(
            port.read(&mut buf).unwrap(),
            0,
            "nothing yet: a timed-out read"
        );
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        master.write_all(b"ping\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let n = port.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping\n", "raw: the newline is kept as it is");
        port.write_all(b"pong").unwrap();
        let n = master.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"pong");
        assert!(super::open("/nonexistent/tty", 9600).is_err());
    }
}
