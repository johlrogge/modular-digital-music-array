# Bandcamp Configuration

This document covers how to configure Bandcamp integration on an MDMA unit. Two things need configuring: browser cookies (for authenticated downloads) and the Bandcamp username.

## What Needs Configuring

- **Cookies file** - Exported from your browser after logging in to Bandcamp. Required for the MDMA unit to authenticate purchases and access your collection. Accepted formats: JSON (Cookie Quick Manager) or Netscape/txt (get-cookies.txt).
- **Username** - Your Bandcamp username. Default: `johlyroger`.

## Exporting Cookies from Browser

### Firefox: Cookie Quick Manager (JSON format)

1. Install the [Cookie Quick Manager](https://addons.mozilla.org/en-US/firefox/addon/cookie-quick-manager/) extension.
2. Log in to `https://bandcamp.com`.
3. Click the Cookie Quick Manager icon in the toolbar.
4. In the search box, type `bandcamp.com` to filter cookies.
5. Click the export icon and choose **Export as JSON**.
6. Save the file (e.g. `cookies.json`).

Make sure the exported file contains at minimum the `identity` cookie for `bandcamp.com`.

### Chrome: Get cookies.txt (Netscape format)

1. Install the [Get cookies.txt LOCALLY](https://chrome.google.com/webstore/detail/get-cookiestxt-locally/cclelndahbckbenkjhflpdbgdldlbecc) extension.
2. Log in to `https://bandcamp.com`.
3. Navigate to `https://bandcamp.com` (must be on the domain).
4. Click the extension icon.
5. Click **Export** to download `cookies.txt` in Netscape format.

## Preferred Method: Upload via Web Console

1. Open `http://mdma-909.local` in your browser.
2. Navigate to the **Bandcamp Configuration** section.
3. Click **Choose File** (or similar) and select your exported cookie file (`.json` or `.txt`).
4. Enter your Bandcamp username (default: `johlyroger`).
5. Click **Configure**.

The console validates and installs the cookie file automatically.

## Manual Alternative: SCP to Pi

If the web console is unavailable, copy the cookie file directly:

```bash
# Copy cookie file to Pi
scp -4 -i ~/.ssh/mdma_pi cookies.json admin@mdma-909.local:/tmp/

# Move to config location
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local \
  sudo mv /tmp/cookies.json /etc/mdma/bandcamp-cookies.json
```

For a Netscape `.txt` file, substitute `bandcamp-cookies.json` with `bandcamp-cookies.txt` as appropriate for your setup.

## Configure Username Manually

Edit the Bandcamp configuration file on the Pi:

```bash
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local
sudo nano /etc/mdma/bandcamp.conf
```

Set or update the `username` field:

```
username=johlyroger
```

Save and exit. Restart the Bandcamp source service if needed:

```bash
sudo sv restart mdma-bandcamp
```

## Verify Configuration

Check that the Bandcamp source is running and authenticated:

```bash
mdma source status bandcamp
```

Expected output shows the source as `running` with a valid session.

## Troubleshooting

### Expired cookies

**Symptom:** Authentication errors or "403 Forbidden" when accessing purchases.

**Fix:** Re-export cookies from your browser (you must be logged in at the time of export) and re-upload via the web console or SCP method above. Cookies typically expire after 30-90 days or when you log out.

### Missing identity cookie

**Symptom:** Source starts but cannot access your collection. Logs show "missing identity" or similar.

**Fix:** Ensure the exported cookie file contains the `identity` cookie for `bandcamp.com`. In Cookie Quick Manager, verify this cookie is present before exporting. In the Netscape format, look for a line containing `identity` under `.bandcamp.com`.

### Wrong domain

**Symptom:** Cookies present but not recognized.

**Fix:** When using Cookie Quick Manager, filter strictly to `bandcamp.com` (not subdomains). When using get-cookies.txt, make sure you are on `https://bandcamp.com` when you export — not an artist subdomain like `artist.bandcamp.com`.
