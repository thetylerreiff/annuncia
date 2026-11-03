// The download page: the newest stable version comes from the update feed on this site, the same
// one installed copies read. Both buttons link straight to a disk image (GitHub redirects the
// fixed release URLs to the file), so nothing opens on GitHub. Nothing downloads until a
// button is pressed.

const SPARKLE = 'http://www.andymatuschak.org/xml-namespaces/sparkle';

const meta = document.getElementById('dlMeta');
const button = document.getElementById('dlButton');
const note = document.getElementById('dlNote');

async function latest() {
  const response = await fetch('/appcast.xml', { cache: 'no-cache' });
  if (!response.ok) return null;
  const feed = new DOMParser().parseFromString(await response.text(), 'application/xml');
  const item = feed.querySelector('item');
  if (!item) return null;
  const version = item.getElementsByTagNameNS(SPARKLE, 'shortVersionString')[0]?.textContent;
  const date = new Date(item.querySelector('pubDate')?.textContent ?? '');
  return { version, date };
}

latest()
  .catch(() => null)
  .then((release) => {
    if (!release) {
      // No stable release yet: leave only the nightly rather than a download that would fail.
      button.hidden = true;
      note.textContent = 'The first stable release is on its way. Until then, the nightly build is the way to try Annuncia.';
      return;
    }
    const when = Number.isNaN(release.date.valueOf())
      ? ''
      : ` · ${release.date.toLocaleDateString('en-US', { month: 'long', day: 'numeric', year: 'numeric' })}`;
    meta.textContent = `Version ${release.version}${when} · macOS 15 Sequoia or later · Apple silicon`;
  });
