use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use ooblin_core::amt::{Client, Connection, Sol, wsman};
use ooblin_core::{Error, Target};

use crate::scrub::scrub;
use crate::{Amt, PASSWORD};

/// Every class MeshCommander or go-wsman-messages knows of. A crawl is cheap, so we would rather ask for one too many.
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

/// Records `target` into `fixture`, unattended and without changing anything on it, and tells how it goes on stderr.
pub async fn record(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    let mut device = Connection::new(target, None)?;
    crawl(&mut device, &fixture.join("wsman")).await?;
    eprintln!("Certificate {}", device.fingerprint().unwrap_or_default());

    // The reads of the app that call methods (the clock, accounts, the logs) go through a forwarding mock, which keeps
    // each reply under the name its replay will look for.
    eprint!("methods  recording…");
    let proxy = Amt::forward(device, target, fixture).await;
    let mut client = Client::new(&proxy.target(PASSWORD), None)?;
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

    // A short SOL, KVM and IDER session as the client of the app has them, through the same mock, saved as transcripts.
    for name in ["sol", "kvm", "ider"] {
        eprint!("{name:>6}  recording…");
        let session = async {
            match name {
                // We press Enter once to get a prompt.
                "sol" => {
                    let sol = client.sol().await?;
                    sol.write(b"\r").await?;
                    listen(&sol, tokio::time::sleep(Duration::from_secs(3))).await
                }
                // WARNING: We fetch one full frame, which is a screenshot: mind what is on the screen!
                "kvm" => {
                    let kvm = client.kvm().await?;

                    // AMT 19 first says only how large its screen is: a frame of a size alone.
                    let (started, mut frame) = (Instant::now(), kvm.frame().await);
                    while frame.as_ref().is_ok_and(|frame| frame.len() == 4) && started.elapsed().as_secs() < 10 {
                        frame = kvm.frame().await;
                    }
                    kvm.close().await;
                    frame.map(drop)
                }
                // We put an empty disc in for the next boot of the device and take it out at once, so that no drive
                // shows up on a host that keeps running.
                _ => {
                    let disc = tempfile::tempfile()?;
                    disc.set_len(2048)?;
                    client.ider(disc).await.map(drop)
                }
            }
        };

        // A session the device refuses (not enabled, say) is skipped, as the replay refuses it the same way.
        match session.await {
            Ok(()) => save(&proxy, fixture, name).await?,
            Err(error) => eprintln!("\r{name:>6}  skipped, {error}"),
        }
    }

    Ok(())
}

/// Records only a serial console session into `fixture`, for as long as it takes to make the device write something
/// (mount a disk image, say), until Enter is pressed here.
pub async fn record_sol(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    let proxy = Amt::forward(Connection::new(target, None)?, target, fixture).await;
    let sol = Client::new(&proxy.target(PASSWORD), None)?.sol().await?;
    eprint!("   sol  recording, press Enter to stop");

    // The terminal hands us the line once Enter is pressed.
    listen(&sol, tokio::io::AsyncReadExt::read_u8(&mut tokio::io::stdin())).await?;

    std::fs::create_dir_all(fixture)?;
    save(&proxy, fixture, "sol").await
}

/// Reads everything the device writes to its serial console until `done`.
async fn listen(sol: &Sol, done: impl Future) -> Result<(), Error> {
    tokio::pin!(done);
    loop {
        tokio::select! {
            read = sol.read() => drop(read?),
            _ = &mut done => break,
        }
    }
    sol.close().await;

    Ok(())
}

/// Saves the session `proxy` forwarded last as the transcript `name` in `fixture`.
async fn save(proxy: &Amt, fixture: &Path, name: &str) -> anyhow::Result<()> {
    let transcript = proxy.forwarded().await;
    transcript.save(&fixture.join(format!("{name}.transcript")))?;
    eprintln!("\r{name:>6}  {} bytes\x1b[K", transcript.0.iter().map(|(_, data)| data.len()).sum::<usize>());

    Ok(())
}

/// Crawls `device` into `directory`, scrubbed, and returns how many classes answered: for every class we know of a
/// WS-Transfer Get and a WS-Enumeration Enumerate and Pull of all its pages, which are both read-only by specification.
/// A class the device lacks answers with a fault and we skip it, as the replay answers it the same way.
pub async fn crawl(device: &mut Connection, directory: &Path) -> anyhow::Result<usize> {
    std::fs::create_dir_all(directory)?;
    let save = |class: &str, action: &str, xml: &str| {
        std::fs::write(directory.join(format!("{class}.{action}.xml")), scrub(xml))
    };

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

        // The pages go in `Class.Pull.xml`, `Class.Pull.2.xml` and so on, as the replay serves them in turn.
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

/// A fault means the device does not have this, anything else (unreachable, wrong password) ends the crawl.
async fn fetch(device: &mut Connection, envelope: String) -> Result<Option<String>, Error> {
    match device.post(envelope).await {
        Ok(xml) => Ok(Some(xml)),
        Err(Error::Fault(_) | Error::Http(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
