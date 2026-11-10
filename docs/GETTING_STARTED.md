# Getting started with NightBloom

NightBloom makes false-colour astronomy art from public Hubble, JWST and survey
images. It runs locally. **Aladin, Python, Node.js and Rust are not required to use
the Windows desktop package.**

## Install on Windows

Use the **Windows x64 installer** from this repository's Releases page, or download
the **portable ZIP** and extract the entire folder before opening
`astroimp-desktop.exe`. Keep the `frontend` folder beside the portable executable;
running it inside the ZIP or copying only the EXE will not work.

Use **Windows 10 or 11 (64-bit)**. The desktop requires
**Microsoft Edge WebView2 Runtime** and a WebGL2-capable
graphics driver. The installer can download WebView2 if it is missing, so its
first installation may require internet access. Portable users can obtain the
[Evergreen WebView2 Runtime from Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/).
This preview is unsigned: Windows may show an unknown-publisher/SmartScreen warning.
Only use downloads from the repository owner and verify their checksums. Do not
disable your antivirus or firewall.

On first launch, choose a **library folder** on a drive with room for FITS
downloads. A small project can still download hundreds of megabytes; larger
compositions can use gigabytes. Your projects, source images, exports and browser
cache stay in that library. The download contains **no personal projects or
library data**.

## Make a first image

1. Choose **New project**, then search for a target such as `M42`, or enter sky
   coordinates. Review the frame and capture it.
2. On **Sky**, select a few images, preferably different filters. Survey coverage
   varies; no single survey covers every target.
3. Choose **Build**. The first build downloads and aligns the source images.
   Later builds reuse cached downloads. Jobs shows progress; alignment time
   estimates improve as comparable work is measured.
4. In **Edit**, choose layer colours and adjust the look. Edits save automatically.
5. Use **Export** to save an image. Export resolution is limited by the actual
   source images, supported projections and available memory.

Existing built projects can be edited and exported offline when their required
source files are present. New searches and uncached downloads need the relevant
remote services. An exported image is not a project backup: back up the entire
library while NightBloom is closed.

## Optional: Aladin Desktop

Aladin lets you explore the sky and send the current view to NightBloom. It is
**optional**; NightBloom's own search and coordinate entry work without it.

1. Visit the [official CDS Aladin website](https://aladin.cds.unistra.fr/) and
   follow its **Aladin Desktop** download instructions. Choose the Windows package
   with Java included when offered. **Aladin Lite**, the browser atlas, is not
   the desktop application's SAMP connection.
2. Open Aladin Desktop and load a sky view.
3. Enable **Connect with SAMP** in Aladin. Keep exactly one Aladin instance
   connected, running as the same Windows user as NightBloom.
4. In NightBloom, choose **New project > Capture from Aladin**.
   Frame and rotate the sky view in Aladin, then capture it.

As of **10 November 2026**, CDS's official website reports a cybersecurity incident
and degraded services; its normal Desktop download pages may be unavailable.
Follow the official site's recovery notices rather than using an unverified
installer mirror. You can use NightBloom without Aladin while downloads recover.

## Troubleshooting

**CDS/MAST unavailable:** check the warning tooltip and the service's own status.
NightBloom has independent alternatives for some surveys, not every archive.
Previously built projects remain usable. A remote outage does not imply Windows
Defender is blocking you.

**Aladin not found:** use Desktop, enable Connect with SAMP, and run both
applications under the same Windows account. Close extra Aladin instances.
Neither application needs an inbound public-network firewall exception for the
local SAMP connection.

**Port already in use:** NightBloom normally uses `127.0.0.1:8766`. Close a duplicate
NightBloom instance or choose a different port with `--port`; do not terminate
unrelated applications.

**Blank/unsupported canvas:** install WebView2 and update your graphics driver.
WebGL2 is required; remote desktops or disabled GPU acceleration may affect it.

**Reporting a problem:** include the app version, Windows version, steps to
reproduce and the visible error/job activity. Do not post your whole library,
private paths or SAMP lock file.

## Verify a download

Download `SHA256SUMS.txt` beside the package, then compare its entry with:

```powershell
Get-FileHash .\NightBloom_0.1.0_x64-setup.exe -Algorithm SHA256
# Or the portable package:
Get-FileHash .\NightBloom_0.1.0_x64-portable.zip -Algorithm SHA256
```

Checksums detect an incomplete or changed download; they do not replace a trusted
download source or a publisher signature.
