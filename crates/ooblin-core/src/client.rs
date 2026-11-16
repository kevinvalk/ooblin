use std::ops::Range;

use crate::redfish::BmcStatus;
use crate::{
    Error, Event, Flavor, Hardware, Info, Input, Panel, Power, PowerAction, Result, Sensor, Target, amt, ipmi, redfish,
    supermicro,
};

#[allow(clippy::large_enum_variant, reason = "we hold a few at most, and clippy only minds without `verify`")]
pub enum Client {
    Amt(amt::Client),
    Supermicro(supermicro::Client),
    Ipmi(ipmi::Client),
    Redfish(redfish::Client),
}

pub enum Kvm {
    Amt(amt::Kvm),
    Supermicro(supermicro::Kvm),
}

pub enum Sol {
    Amt(amt::Sol),
    Ipmi(ipmi::Sol),
}

/// A disc in the device's drive: AMT's IDER, or a BMC's virtual media.
#[expect(clippy::large_enum_variant, reason = "we hold one at a time")]
pub enum Media {
    Amt(amt::Ider),
    Supermicro(supermicro::Media),
}

// `$call` on whichever of `$variants` `$value` holds, named `$it`, and `Unsupported` for those it is `not` for. We name
// every variant, so a new flavor has to be asked about each.
macro_rules! either {
    ($value:expr, $it:ident => $call:expr, $($variant:ident)|+ $(, not $($without:ident)|+)?) => {
        match $value {
            $(Self::$variant($it) => $call,)+
            $($(Self::$without(_) => Err(Error::Unsupported),)+)?
        }
    };
}

impl Client {
    /// Logs in as `flavor`, or when we do not know it as what the host turns out to be: see `guess`.
    pub async fn login(target: &Target, pin: Option<String>, flavor: Option<Flavor>) -> Result<(Self, Info)> {
        match flavor {
            Some(flavor) => Self::login_as(target, pin, flavor).await,
            None => Self::guess(target, pin).await,
        }
    }

    async fn login_as(target: &Target, pin: Option<String>, flavor: Flavor) -> Result<(Self, Info)> {
        Ok(match flavor {
            Flavor::Amt => {
                let mut client = amt::Client::new(target, pin)?;
                let info = client.info().await?;
                (Self::Amt(client), info)
            }
            Flavor::Supermicro => {
                let mut client = supermicro::Client::new(target, pin)?;
                let info = client.info().await?;
                (Self::Supermicro(client), info)
            }
            Flavor::Ipmi => {
                let client = ipmi::Client::new(target);
                let info = client.info().await?;
                (Self::Ipmi(client), info)
            }
            Flavor::Redfish => {
                let client = redfish::Client::new(target, pin)?;
                let info = client.info().await?;
                (Self::Redfish(client), info)
            }
        })
    }

    async fn guess(target: &Target, pin: Option<String>) -> Result<(Self, Info)> {
        // AMT goes first, and we only go on when it cannot be reached. A host that names a port never can be, because
        // the URL carries AMT's own port.
        let amt_error = match Self::login_as(target, pin.clone(), Flavor::Amt).await {
            Err(error @ (Error::Transport(_) | Error::Timeout | Error::Http(_))) => error,
            login => return login,
        };

        // Then a Supermicro, but ONLY if the host answers Redfish without a login. That way no web server that is not
        // a BMC gets the password, and another vendor's Redfish says it is not a Supermicro.
        let has_redfish = redfish::probe(&target.authority()).await;
        if has_redfish {
            match Self::login_as(target, pin.clone(), Flavor::Supermicro).await {
                Err(Error::Protocol(_)) => {}
                login => return login,
            }
        }

        // Then IPMI, whose login never sends the password and which has a serial console, and where IPMI is off the
        // Redfish that answered. When nothing answers at all we say why AMT did not.
        match Self::login_as(target, None, Flavor::Ipmi).await {
            Err(Error::Timeout | Error::Transport(_)) if has_redfish => {
                Self::login_as(target, pin, Flavor::Redfish).await
            }
            Err(Error::Timeout | Error::Transport(_)) => Err(amt_error),
            login => login,
        }
    }

    pub fn amt(&mut self) -> Result<&mut amt::Client> {
        let Self::Amt(client) = self else { return Err(Error::Unsupported) };
        Ok(client)
    }

    pub fn supermicro(&mut self) -> Result<&mut supermicro::Client> {
        let Self::Supermicro(client) = self else { return Err(Error::Unsupported) };
        Ok(client)
    }

    pub fn ipmi(&mut self) -> Result<&mut ipmi::Client> {
        let Self::Ipmi(client) = self else { return Err(Error::Unsupported) };
        Ok(client)
    }

    pub async fn power(&mut self) -> Result<Power> {
        either!(self, client => client.power().await, Amt | Supermicro | Ipmi | Redfish)
    }

    pub async fn set_power(&mut self, action: PowerAction) -> Result<()> {
        either!(self, client => client.set_power(action).await, Amt | Supermicro | Ipmi | Redfish)
    }

    pub async fn hardware(&mut self) -> Result<Hardware> {
        // NOTE: What IPMI says of the hardware is its FRU, see `ipmi::Client::fru`.
        either!(self, client => client.hardware().await, Amt | Supermicro | Redfish, not Ipmi)
    }

    pub async fn event_log(&mut self) -> Result<Vec<Event>> {
        either!(self, client => client.event_log().await, Amt | Supermicro | Ipmi | Redfish)
    }

    pub async fn bmc_status(&mut self) -> Result<BmcStatus> {
        either!(self, client => client.status().await, Supermicro | Redfish, not Amt | Ipmi)
    }

    pub async fn sensors(&mut self) -> Result<Vec<Sensor>> {
        either!(self, client => client.sensors().await, Supermicro | Ipmi | Redfish, not Amt)
    }

    pub async fn panel(&mut self) -> Result<Panel> {
        either!(self, client => client.panel().await, Supermicro | Ipmi | Redfish, not Amt)
    }

    /// Lights a BMC's identify light until it is turned off, or turns it off.
    pub async fn identify(&mut self, is_on: bool) -> Result<()> {
        either!(self, client => client.identify(is_on).await, Supermicro | Ipmi | Redfish, not Amt)
    }

    pub async fn kvm(&mut self) -> Result<Kvm> {
        match self {
            Self::Amt(client) => client.kvm().await.map(Kvm::Amt),
            Self::Supermicro(client) => client.kvm().await.map(Kvm::Supermicro),
            Self::Ipmi(_) | Self::Redfish(_) => Err(Error::Unsupported),
        }
    }

    pub async fn sol(&mut self) -> Result<Sol> {
        match self {
            Self::Amt(client) => client.sol().await.map(Sol::Amt),
            Self::Supermicro(client) => client.sol().await.map(Sol::Ipmi),
            Self::Ipmi(client) => client.sol().await.map(Sol::Ipmi),
            Self::Redfish(_) => Err(Error::Unsupported),
        }
    }

    /// Puts `image` in the device's drive, see `amt::Client::ider` and `supermicro::Client::media`.
    pub async fn mount(&mut self, image: std::fs::File) -> Result<Media> {
        match self {
            Self::Amt(client) => client.ider(image).await.map(Media::Amt),
            Self::Supermicro(client) => client.media(image).await.map(Media::Supermicro),
            Self::Ipmi(_) | Self::Redfish(_) => Err(Error::Unsupported),
        }
    }

    /// The device's own web pages, where what we do not do can be done.
    pub fn web_pages(&self) -> String {
        match self {
            Self::Amt(client) => client.web_pages(),
            Self::Supermicro(client) => client.web_pages(),
            Self::Ipmi(client) => client.web_pages(),
            Self::Redfish(client) => client.web_pages(),
        }
    }
}

impl Kvm {
    /// The next change on screen, see `amt::Kvm::frame`.
    pub async fn frame(&self) -> Result<Vec<u8>> {
        either!(self, kvm => kvm.frame().await, Amt | Supermicro)
    }

    pub async fn input(&self, inputs: Vec<Input>) -> Result<()> {
        either!(self, kvm => kvm.input(inputs).await, Amt | Supermicro)
    }

    pub async fn close(&self) {
        match self {
            Self::Amt(kvm) => kvm.close().await,
            Self::Supermicro(kvm) => kvm.close().await,
        }
    }
}

impl Media {
    /// Answers the device until it is done with the disc or `eject` takes it out, see `amt::Ider::serve`.
    pub async fn serve(self, read: impl FnMut(Range<u64>), eject: impl Future<Output = ()>) -> Result<()> {
        match self {
            Self::Amt(ider) => ider.serve(read, eject).await,
            Self::Supermicro(media) => media.serve(read, eject).await,
        }
    }
}

impl Sol {
    /// What the device wrote to its serial port since the last read, or nothing after a second.
    pub async fn read(&self) -> Result<Vec<u8>> {
        either!(self, sol => sol.read().await, Amt | Ipmi)
    }

    pub async fn write(&self, bytes: &[u8]) -> Result<()> {
        either!(self, sol => sol.write(bytes).await, Amt | Ipmi)
    }

    pub async fn close(&self) {
        match self {
            Self::Amt(sol) => sol.close().await,
            Self::Ipmi(sol) => sol.close().await,
        }
    }
}
