//! Records a device into a fixture, unattended and without changing anything on it:
//!
//! - WS-Man: for every class we know of, a WS-Transfer Get and a WS-Enumeration Enumerate/Pull, all pages of it. Both are
//!   read-only by spec. Classes the device lacks answer with a fault and we skip them, the replay answers those the same
//!   way. Then the app's own reads that call methods (the clock, accounts, the logs), through a forwarding mock that
//!   keeps each reply under the name its replay will look for.
//! - Redirection: a short SOL, KVM and IDER session, saved as transcripts. SOL presses Enter once to get a prompt, KVM
//!   fetches one full frame (a screenshot, mind what's on screen), IDER only opens the session, so no drive ever shows
//!   up on the host. Message layouts from MeshCommander (Apache-2.0).

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use amt_core::{Client, Connection, Error, Redirection, Session, Target, wsman};

use crate::scrub::scrub;
use crate::transcript::{Direction, Transcript};
use crate::{Amt, PASSWORD, USER};

/// Every class MeshCommander or go-wsman-messages knows of. A crawl is cheap, so we'd rather ask for one too many.
const CLASSES: &[&str] = &[
    "AMT_8021xCredentialContext",
    "AMT_8021XProfile",
    "AMT_ActiveFilterStatistics",
    "AMT_AgentPresenceCapabilities",
    "AMT_AgentPresenceInterfacePolicy",
    "AMT_AgentPresenceService",
    "AMT_AgentPresenceWatchdog",
    "AMT_AgentPresenceWatchdogAction",
    "AMT_AlarmClockService",
    "AMT_AssetTable",
    "AMT_AssetTableService",
    "AMT_AuditLog",
    "AMT_AuditPolicyRule",
    "AMT_AuthorizationService",
    "AMT_BootCapabilities",
    "AMT_BootSettingData",
    "AMT_ComplexFilterEntryBase",
    "AMT_CRL",
    "AMT_CryptographicCapabilities",
    "AMT_EACCredentialContext",
    "AMT_EndpointAccessControlService",
    "AMT_EnvironmentDetectionInterfacePolicy",
    "AMT_EnvironmentDetectionSettingData",
    "AMT_EthernetPortSettings",
    "AMT_EventLogEntry",
    "AMT_EventManagerService",
    "AMT_EventSubscriber",
    "AMT_FilterEntryBase",
    "AMT_FilterInSystemDefensePolicy",
    "AMT_GeneralSettings",
    "AMT_GeneralSystemDefenseCapabilities",
    "AMT_Hdr8021Filter",
    "AMT_HeuristicPacketFilterInterfacePolicy",
    "AMT_HeuristicPacketFilterSettings",
    "AMT_HeuristicPacketFilterStatistics",
    "AMT_Implementation",
    "AMT_InterfacePolicy",
    "AMT_IPHeadersFilter",
    "AMT_KerberosSettingData",
    "AMT_ManagementPresenceRemoteSAP",
    "AMT_MessageLog",
    "AMT_MPSUsernamePassword",
    "AMT_NetworkFilter",
    "AMT_NetworkPortDefaultSystemDefensePolicy",
    "AMT_NetworkPortSystemDefenseCapabilities",
    "AMT_NetworkPortSystemDefensePolicy",
    "AMT_PCIDevice",
    "AMT_PETCapabilities",
    "AMT_PETFilterForTarget",
    "AMT_PETFilterSetting",
    "AMT_PolicySetAppliesToElement",
    "AMT_ProvisioningCertificateHash",
    "AMT_PublicKeyCertificate",
    "AMT_PublicKeyManagementCapabilities",
    "AMT_PublicKeyManagementService",
    "AMT_PublicPrivateKeyPair",
    "AMT_RedirectionService",
    "AMT_RemoteAccessCapabilities",
    "AMT_RemoteAccessCredentialContext",
    "AMT_RemoteAccessPolicyAppliesToMPS",
    "AMT_RemoteAccessPolicyRule",
    "AMT_RemoteAccessService",
    "AMT_SetupAndConfigurationService",
    "AMT_SNMPEventSubscriber",
    "AMT_StateTransitionCondition",
    "AMT_SystemDefensePolicy",
    "AMT_SystemDefensePolicyInService",
    "AMT_SystemDefenseService",
    "AMT_SystemPowerScheme",
    "AMT_ThirdPartyDataStorageAdministrationService",
    "AMT_ThirdPartyDataStorageService",
    "AMT_TimeSynchronizationService",
    "AMT_TLSCredentialContext",
    "AMT_TLSProtocolEndpoint",
    "AMT_TLSProtocolEndpointCollection",
    "AMT_TLSSettingData",
    "AMT_TrapTargetForService",
    "AMT_UserInitiatedConnectionService",
    "AMT_WebUIService",
    "AMT_WiFiPortConfigurationService",
    "CIM_AbstractIndicationSubscription",
    "CIM_Account",
    "CIM_AccountManagementCapabilities",
    "CIM_AccountManagementService",
    "CIM_AccountOnSystem",
    "CIM_AdminDomain",
    "CIM_AlertIndication",
    "CIM_AssignedIdentity",
    "CIM_AssociatedPowerManagementService",
    "CIM_AssociatedSensor",
    "CIM_AuthenticationService",
    "CIM_AuthorizationService",
    "CIM_Battery",
    "CIM_BIOSElement",
    "CIM_BIOSFeature",
    "CIM_BIOSFeatureBIOSElements",
    "CIM_BootConfigSetting",
    "CIM_BootService",
    "CIM_BootSettingData",
    "CIM_BootSourceSetting",
    "CIM_Capabilities",
    "CIM_Card",
    "CIM_Chassis",
    "CIM_Chip",
    "CIM_Collection",
    "CIM_Component",
    "CIM_ComputerSystem",
    "CIM_ComputerSystemPackage",
    "CIM_ConcreteComponent",
    "CIM_ConcreteDependency",
    "CIM_Controller",
    "CIM_CoolingDevice",
    "CIM_Credential",
    "CIM_CredentialContext",
    "CIM_CredentialManagementService",
    "CIM_Dependency",
    "CIM_DeviceSAPImplementation",
    "CIM_ElementCapabilities",
    "CIM_ElementConformsToProfile",
    "CIM_ElementLocation",
    "CIM_ElementSettingData",
    "CIM_ElementSoftwareIdentity",
    "CIM_ElementStatisticalData",
    "CIM_EnabledLogicalElement",
    "CIM_EnabledLogicalElementCapabilities",
    "CIM_EthernetPort",
    "CIM_Fan",
    "CIM_FilterCollection",
    "CIM_FilterCollectionSubscription",
    "CIM_HostedAccessPoint",
    "CIM_HostedDependency",
    "CIM_HostedService",
    "CIM_Identity",
    "CIM_IEEE8021xCapabilities",
    "CIM_IEEE8021xSettings",
    "CIM_Indication",
    "CIM_IndicationService",
    "CIM_InstalledSoftwareIdentity",
    "CIM_KVMRedirectionSAP",
    "CIM_LANEndpoint",
    "CIM_ListenerDestination",
    "CIM_ListenerDestinationWSManagement",
    "CIM_Location",
    "CIM_Log",
    "CIM_LogEntry",
    "CIM_LogicalDevice",
    "CIM_LogicalElement",
    "CIM_LogicalPort",
    "CIM_LogicalPortCapabilities",
    "CIM_LogManagesRecord",
    "CIM_ManagedCredential",
    "CIM_ManagedElement",
    "CIM_ManagedSystemElement",
    "CIM_MediaAccessDevice",
    "CIM_MemberOfCollection",
    "CIM_Memory",
    "CIM_MessageLog",
    "CIM_NetworkPort",
    "CIM_NetworkPortCapabilities",
    "CIM_NetworkPortConfigurationService",
    "CIM_OrderedComponent",
    "CIM_OwningCollectionElement",
    "CIM_OwningJobElement",
    "CIM_PCIController",
    "CIM_PhysicalComponent",
    "CIM_PhysicalElement",
    "CIM_PhysicalElementLocation",
    "CIM_PhysicalFrame",
    "CIM_PhysicalMemory",
    "CIM_PhysicalPackage",
    "CIM_Policy",
    "CIM_PolicyAction",
    "CIM_PolicyCondition",
    "CIM_PolicyInSystem",
    "CIM_PolicyRule",
    "CIM_PolicyRuleInSystem",
    "CIM_PolicySet",
    "CIM_PolicySetAppliesToElement",
    "CIM_PolicySetInSystem",
    "CIM_PowerManagementCapabilities",
    "CIM_PowerManagementService",
    "CIM_PowerSupply",
    "CIM_Privilege",
    "CIM_PrivilegeManagementCapabilities",
    "CIM_PrivilegeManagementService",
    "CIM_ProcessIndication",
    "CIM_Processor",
    "CIM_Product",
    "CIM_ProtocolEndpoint",
    "CIM_ProvidesServiceToElement",
    "CIM_Realizes",
    "CIM_RecordForLog",
    "CIM_RecordLog",
    "CIM_RedirectionService",
    "CIM_ReferencedProfile",
    "CIM_RegisteredProfile",
    "CIM_RemoteAccessAvailableToElement",
    "CIM_RemoteIdentity",
    "CIM_RemotePort",
    "CIM_RemoteServiceAccessPoint",
    "CIM_Role",
    "CIM_RoleBasedAuthorizationService",
    "CIM_RoleBasedManagementCapabilities",
    "CIM_RoleLimitedToTarget",
    "CIM_SAPAvailableForElement",
    "CIM_SecurityService",
    "CIM_Sensor",
    "CIM_Service",
    "CIM_ServiceAccessBySAP",
    "CIM_ServiceAccessPoint",
    "CIM_ServiceAffectsElement",
    "CIM_ServiceAvailable",
    "CIM_ServiceAvailableToElement",
    "CIM_ServiceSAPDependency",
    "CIM_ServiceServiceDependency",
    "CIM_SettingData",
    "CIM_SharedCredential",
    "CIM_SoftwareElement",
    "CIM_SoftwareFeature",
    "CIM_SoftwareFeatureSoftwareElements",
    "CIM_SoftwareIdentity",
    "CIM_StatisticalData",
    "CIM_StorageExtent",
    "CIM_System",
    "CIM_SystemBIOS",
    "CIM_SystemComponent",
    "CIM_SystemDevice",
    "CIM_SystemPackaging",
    "CIM_Tachometer",
    "CIM_UseOfLog",
    "CIM_Watchdog",
    "CIM_WiFiEndpoint",
    "CIM_WiFiEndpointCapabilities",
    "CIM_WiFiEndpointSettings",
    "CIM_WiFiPort",
    "CIM_WiFiPortCapabilities",
    "IPS_AdminProvisioningRecord",
    "IPS_AlarmClockOccurrence",
    "IPS_ClientProvisioningRecord",
    "IPS_HostBasedSetupService",
    "IPS_HostBootReason",
    "IPS_HostIPSettings",
    "IPS_HTTPProxyAccessPoint",
    "IPS_HTTPProxyService",
    "IPS_IderSessionUsingPort",
    "IPS_IEEE8021xSettings",
    "IPS_IPv6PortSettings",
    "IPS_KVMRedirectionSettingData",
    "IPS_KvmSessionUsingPort",
    "IPS_LANEndpoint",
    "IPS_ManualProvisioningRecord",
    "IPS_OptInService",
    "IPS_PowerManagementService",
    "IPS_ProvisioningAuditRecord",
    "IPS_ProvisioningRecordLog",
    "IPS_RasSessionUsingPort",
    "IPS_ScreenConfigurationService",
    "IPS_ScreenSettingData",
    "IPS_SecIOService",
    "IPS_SessionUsingPort",
    "IPS_SolSessionUsingPort",
    "IPS_TLSProvisioningRecord",
    "IPS_WatchDogAction",
];

/// Records `target` into `fixture`, telling how it goes on stderr.
pub async fn record(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    let mut device = Connection::new(target, None)?;
    crawl(&mut device, &fixture.join("wsman")).await?;
    let pin = device.fingerprint();
    eprintln!("Certificate {}", pin.as_deref().unwrap_or_default());

    eprint!("methods  recording…");
    let proxy = Amt::forward(device, fixture).await;
    let mut client = Client::new(&Target { wsman_port: proxy.port, ..Target::new("127.0.0.1", USER, PASSWORD) }, None)?;
    let reads = [
        client.status().await.err(),
        client.settings().await.err(),
        client.event_log().await.err(),
        client.audit_log().await.err(),
    ];
    match reads.into_iter().flatten().next() {
        None => eprintln!("\rmethods  clock, accounts, event log and audit log"),
        Some(error) => eprintln!("\rmethods  incomplete, {error}"),
    }

    for (session, name) in [(Session::Sol, "sol"), (Session::Kvm, "kvm"), (Session::Ider, "ider")] {
        eprint!("{name:>6}  recording…");
        // A session the device refuses (not enabled, say) is skipped, the replay refuses it the same way.
        match Recording::run(target, pin.clone(), session).await {
            Ok(transcript) => {
                transcript.save(&fixture.join(format!("{name}.transcript")))?;
                eprintln!("\r{name:>6}  {} bytes", transcript.0.iter().map(|(_, data)| data.len()).sum::<usize>());
            }
            Err(error) => eprintln!("\r{name:>6}  skipped, {error}"),
        }
    }
    Ok(())
}

/// Crawls `device` into `dir`, scrubbed, and returns how many classes answered.
pub async fn crawl(device: &mut Connection, dir: &Path) -> anyhow::Result<usize> {
    std::fs::create_dir_all(dir)?;
    let save =
        |class: &str, action: &str, xml: &str| std::fs::write(dir.join(format!("{class}.{action}.xml")), scrub(xml));

    let mut answered = 0;
    for (i, class) in CLASSES.iter().enumerate() {
        eprint!("\r WS-Man  {}/{}  {class}\x1b[K", i + 1, CLASSES.len());
        let _ = std::io::stderr().flush();
        let get = fetch(device, wsman::get(class, 0)).await?;
        if let Some(xml) = &get {
            save(class, "Get", xml)?;
        }
        let enumerate = fetch(device, wsman::enumerate(class, 0)).await?;
        let mut context = enumerate.as_deref().map(wsman::parse_enumerate).transpose()?;
        // Pages as `Class.Pull.xml`, `Class.Pull.2.xml` and so on, the replay serves them in turn.
        let mut pages = 0;
        while let Some(current) = context {
            let xml = device.post(wsman::pull(class, &current, 0)).await?;
            context = wsman::parse_pull(&xml)?.next;
            pages += 1;
            save(class, &if pages == 1 { "Pull".to_owned() } else { format!("Pull.{pages}") }, &xml)?;
        }
        answered += usize::from(get.is_some() || pages > 0);
    }
    eprintln!("\r WS-Man  {answered} of {} classes answered\x1b[K", CLASSES.len());
    Ok(answered)
}

/// A fault means the device doesn't have this, anything else (unreachable, wrong password) ends the crawl.
async fn fetch(device: &mut Connection, envelope: String) -> Result<Option<String>, Error> {
    match device.post(envelope).await {
        Ok(xml) => Ok(Some(xml)),
        Err(Error::Fault(_) | Error::Http(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// A redirection session that keeps a transcript of everything after the handshake.
struct Recording {
    redirection: Redirection,
    transcript: Transcript,
}

impl Recording {
    async fn run(target: &Target, pin: Option<String>, session: Session) -> Result<Transcript, Error> {
        let redirection = Redirection::open(target, pin, session).await?;
        let mut recording = Self { redirection, transcript: Transcript::default() };
        match session {
            Session::Sol => recording.sol().await?,
            Session::Kvm => recording.kvm().await?,
            Session::Ider => recording.ider().await?,
        }
        Ok(recording.transcript)
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.transcript.0.push((Direction::Sent, bytes.to_vec()));
        self.redirection.write_all(bytes).await
    }

    async fn receive(&mut self, len: usize) -> Result<Vec<u8>, Error> {
        let bytes = self.redirection.read_exact(len).await?;
        if len > 0 {
            self.transcript.0.push((Direction::Received, bytes.clone()));
        }
        Ok(bytes)
    }

    /// Everything the device sends within `window`, as it arrives.
    async fn listen(&mut self, window: Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + window;
        let mut buf = vec![0; 64 * 1024];
        while let Ok(read) = tokio::time::timeout_at(deadline, self.redirection.read(&mut buf)).await {
            match read? {
                0 => break,
                len => self.transcript.0.push((Direction::Received, buf[..len].to_vec())),
            }
        }
        Ok(())
    }

    /// Start SOL with MeshCommander's settings, press Enter and listen.
    async fn sol(&mut self) -> Result<(), Error> {
        let mut start = vec![0x20, 0, 0, 0, 1, 0, 0, 0];
        // Max tx buffer, then the tx, tx overflow, rx and rx flush timeouts, then the heartbeat.
        for setting in [10000u16, 100, 0, 10000, 100, 0] {
            start.extend_from_slice(&setting.to_le_bytes());
        }
        start.extend_from_slice(&[0, 0, 0, 0]);
        self.send(&start).await?;
        self.receive(23).await?;
        self.send(&[0x27, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0x1B, 0, 0, 0]).await?;
        self.send(&[0x28, 0, 0, 0, 3, 0, 0, 0, 1, 0, b'\r']).await?;
        self.listen(Duration::from_secs(3)).await
    }

    /// Start KVM, do the RFB handshake the way MeshCommander does and fetch one full frame in Raw.
    async fn kvm(&mut self) -> Result<(), Error> {
        self.send(&[0x40, 0, 0, 0, 0, 0, 0, 0]).await?;
        self.receive(8).await?;
        self.receive(12).await?; // AMT 19 says RFB 004.000
        self.send(b"RFB 003.008\n").await?;
        let count = self.receive(1).await?[0];
        self.receive(usize::from(count)).await?;
        self.send(&[1]).await?; // no RFB security, the redirection handshake already authenticated us
        self.receive(4).await?;
        self.send(&[1]).await?; // shared
        let init = self.receive(24).await?;
        self.receive(u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize).await?;
        let (mut width, mut height) = ([init[0], init[1]], [init[2], init[3]]);
        let bytes_per_pixel = usize::from(init[4] / 8);

        // Raw plus the DesktopSize pseudo-encoding, AMT insists on Raw being there.
        self.send(&[2, 0, 0, 2, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x21]).await?;
        // AMT 19 answers the first request with only its desktop size, so we ask again until pixels arrive.
        for _ in 0..3 {
            self.send(&[&[3, 0, 0, 0, 0, 0], &width[..], &height[..]].concat()).await?;
            self.update().await?;
            let mut pixels = false;
            let update = self.receive(3).await?;
            for _ in 0..u16::from_be_bytes([update[1], update[2]]) {
                let rect = self.receive(12).await?;
                let (w, h) = ([rect[4], rect[5]], [rect[6], rect[7]]);
                match i32::from_be_bytes(rect[8..12].try_into().unwrap()) {
                    0 => {
                        let size = usize::from(u16::from_be_bytes(w)) * usize::from(u16::from_be_bytes(h));
                        self.receive(size * bytes_per_pixel).await?;
                        pixels = true;
                    }
                    -223 => (width, height) = (w, h),
                    encoding => return Err(Error::Protocol(format!("unexpected RFB encoding {encoding}"))),
                }
            }
            if pixels {
                return Ok(());
            }
        }
        Err(Error::Protocol("no frame after three requests".into()))
    }

    /// Reads RFB server messages up to the next FramebufferUpdate, AMT 19 rings the Bell before the first one.
    async fn update(&mut self) -> Result<(), Error> {
        loop {
            match self.receive(1).await?[0] {
                0 => return Ok(()), // FramebufferUpdate
                1 => {
                    // SetColourMapEntries
                    let header = self.receive(5).await?;
                    self.receive(usize::from(u16::from_be_bytes([header[3], header[4]])) * 6).await?;
                }
                2 => {} // Bell
                3 => {
                    // ServerCutText
                    let header = self.receive(7).await?;
                    self.receive(u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize).await?;
                }
                other => return Err(Error::Protocol(format!("unexpected RFB server message {other}"))),
            }
        }
    }

    /// Open IDER and keep what the device tells us about it (buffer sizes, versions), then hang up.
    async fn ider(&mut self) -> Result<(), Error> {
        let mut open = vec![0x40, 0, 0, 0, 0, 0, 0, 0];
        open.extend_from_slice(&30000u16.to_le_bytes()); // rx timeout
        open.extend_from_slice(&0u16.to_le_bytes()); // tx timeout
        open.extend_from_slice(&20000u16.to_le_bytes()); // heartbeat
        open.extend_from_slice(&1u32.to_le_bytes()); // version
        self.send(&open).await?;
        let reply = self.receive(30).await?;
        self.receive(usize::from(reply[29])).await?;
        Ok(())
    }
}
