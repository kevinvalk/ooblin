# What it supports

## Intel AMT

By the features in the [AMT Implementation and Reference Guide](https://software.intel.com/sites/manageability/AMT_Implementation_and_Reference_Guide/default.htm) of Intel, as checked against the MS-02 (AMT 19). "Partly" means some of it, see the notes.

| Feature                                      |        | Notes                                                                                                                                                                   |
| -------------------------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Discovery, general info                      | Partly | Finding devices by RMCP ping, version, control mode, host name, certificate. Not: provisioning record, ME capabilities, privacy level.                                  |
| Hardware asset                               | Yes    | System, BIOS, processors, memory, disks, batteries. Not: FRU and media device tables.                                                                                   |
| Remote control (power, boot)                 | Partly | Every power action, booting once to BIOS, PXE, a disk image or with the BIOS on SOL. Not: the boots of One-Click Recovery, local boot sources, BIOS pause, input locks. |
| KVM                                          | Yes    | ZRLE and Raw, 16-bit and 8-bit beyond 4K as AMT wants, keyboard, mouse, typing text, full screen, picking a screen, frames a second. Not: KVM settings.                 |
| SOL                                          | Yes    | xterm.js at 80 by 25, Backspace sent as ^H like MeshCommander.                                                                                                          |
| Storage redirection (IDER)                   | Yes    | A CD-ROM from an ISO, the floppy left empty.                                                                                                                            |
| User consent                                 | Yes    | Choosing when it is needed, and asking for it: the device shows a code that someone there reads out.                                                                    |
| Redirection and KVM switches                 | Yes    | By hand in the settings, sessions open only what is on.                                                                                                                 |
| Event manager (event log)                    | Partly | Reading. Not: export, clear, freeze, filters, alerts, subscriptions.                                                                                                    |
| Access monitor (audit log)                   | Partly | Reading. Not: export, clear, lock, auditing on or off, policy.                                                                                                          |
| Alarm clock                                  | Yes    | Scheduling wake-ups (once, hourly, daily, weekly) and canceling them.                                                                                                   |
| Time synchronization                         | Yes    | Reading the clock, setting it to the one of this computer.                                                                                                              |
| Network administration                       | Partly | Wired IPv4 and IPv6, when it is reachable, the name, DNS registration, ping. Not: wireless.                                                                             |
| Wi-Fi port configuration                     | No     |                                                                                                                                                                         |
| 802.1X profiles                              | No     |                                                                                                                                                                         |
| Access control list (users)                  | No     | Left to the own web pages of AMT, which we link to.                                                                                                                     |
| Certificate management                       | Partly | Listing, adding certificates and trusted roots, saving, deleting. Not: keys, signing requests.                                                                          |
| Transport layer security                     | Partly | TLS only, pinned, its settings shown. Not: changing them or its certificate.                                                                                            |
| Remote platform erase                        | Partly | Turning it and One-Click Recovery on and off where the BIOS allows. Not: erasing (neither on the MS-02).                                                                |
| Environment detection                        | No     |                                                                                                                                                                         |
| Kerberos                                     | No     | Not planned.                                                                                                                                                            |
| Remote access (CIRA), Fast Call for Help     | No     | Not planned: needs a server.                                                                                                                                            |
| WS-Eventing, alerts                          | No     | Not planned: needs a listener.                                                                                                                                          |
| System defense, agent presence               | No     | Not planned.                                                                                                                                                            |
| Power packages (power policy)                | Yes    | Choosing which of the packages of the maker the device uses, and how long AMT stays awake after.                                                                        |
| Third-party storage, web application hosting | No     | Not planned: deprecated.                                                                                                                                                |

## Supermicro BMCs

Through Redfish, the own web pages and HTML5 KVM of the BMC, and IPMI 2.0, as checked against an X10 (BMC firmware 4.07), and the settings and FRU also against an A2SDi-H-TF (4.04):

- status, with the dates of the firmware and the BIOS and the LAN ports of the server
- power, with booting once to BIOS, PXE or a disk image
- the identify light (UID) and the POST code
- hardware and the event log
- sensors (temperatures, fans, voltages, power supplies) every second, read over IPMI as that is five times as fast as the Redfish of the X10
- the KVM, with keyboard, mouse and typing text
- the serial console (SOL)
- a disk image on your machine as a USB CD-ROM drive (the virtual media of the HTML5 viewer)
- settings: fan and mouse mode, NTP, syslog, what the server does when power comes back, the speed of the serial console, the fields of the FRU (which a Supermicro locks, so we unlock it for the write as its own SMCIPMITool does), its services (each on or off and on which port, except IPMI and HTTPS as we speak those) and its network (host name, DHCP or its addresses, DNS server and VLAN)
- a change the BMC refuses says why, in the words of its Redfish

Not: users (left to the own web pages of the BMC, which we link to), the system's model and serial of its BIOS (SMBIOS, which only Supermicro's licensed tools change), or anything else that needs a Supermicro license.

## Any BMC, over IPMI 2.0

Whoever made it (Dell, HPE, Lenovo, ASRock Rack, Gigabyte, OpenBMC, a Supermicro without Redfish), over IPMI alone (RMCP+ on UDP 623, SHA-256 or SHA-1 with AES), as checked against the X10, OpenBMC and the simulator of OpenIPMI:

- the maker, firmware and network port of the BMC
- power, with booting once to BIOS or PXE, and the identify light
- sensors from its SDR (every second) and the event log in words
- its FRU as hardware
- the serial console (SOL)
- settings: its network (DHCP or its addresses, and VLAN), what the server does when power comes back, the speed of the serial console, the chassis, board and product fields of the FRU (rewritten in place and checked by reading them back), and its users to read

Not: a screen or virtual media, as IPMI has neither and every vendor has its own.

## Any BMC, over Redfish

Whoever made it, where IPMI is off or you would rather not speak it, as far as the standard goes (DSP0266), as checked against OpenBMC and the sample server of DMTF:

- the first server, BMC and chassis that its service lists
- status, with the network port of the BMC and those of the server
- power, with booting once to BIOS or PXE (what its Reset allows, where it says so in the action or in a resource of its own)
- the identify light, under either of its names
- hardware: processors, memory, and disks from its storage controllers or simple storage
- the logs of the server
- sensors, each a resource of its own or as its thermal and power resources say them

Not: a screen, a serial console or virtual media (every vendor has its own), nor settings.
