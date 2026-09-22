# naxvoice 0.1.0

Dictation and read-aloud for macOS: hold a key and speak and the polished text
appears at your cursor, or select any text and press a key to hear it read back.

## Requirements

- An Apple Silicon Mac running macOS 11 or later. This release is `aarch64`
  only. No Intel or universal build has been produced or tested.
- An OpenRouter API key, for transcription and cleanup. Read-aloud needs no key
  and no network: the voice runs on your Mac.
- Nothing else. The speech model and voice ship inside the app.

## Installing

1. Open the disk image and drag naxvoice to Applications.
2. Open it. **macOS will block it the first time**, saying it cannot check the
   app for malicious software. That is expected, and step 3 is how you get past
   it.
3. Open System Settings, go to Privacy and Security, scroll down to the message
   about naxvoice, and click **Open Anyway**.
4. Follow the setup screen inside the app. It walks you through the two macOS
   permissions and your API key.

If you are comfortable in Terminal, this does the same thing as steps 2 and 3:

```bash
xattr -dr com.apple.quarantine /Applications/naxvoice.app
```

**Why any of this is necessary:** the app is signed, but with an ad-hoc
signature rather than an Apple Developer ID, and it is not notarised. macOS
treats anything without a Developer ID as unverified, whoever wrote it.

## The two permissions

naxvoice needs two separate macOS grants, and having one does not give you the
other:

- **Input Monitoring** decides whether the dictation key fires at all. Without
  it nothing is recorded and the app looks dead.
- **Accessibility** decides whether text can be pasted. Without it dictation
  records and transcribes, and then nothing appears.

Both are read only when the app starts, so grant them and then restart
naxvoice. The setup screen has a button for that.

## Known limitations

These are measured, not estimated.

- **macOS only.** The Windows clipboard, paste and foreground-window code is
  written, but watching a bare modifier key is not, so the dictation key never
  fires there.
- **Dictation round trip is a little over 2.5 seconds** from key release to
  pasted text, as the Status screen measures it on real use. Most of that is
  the cleanup model, not transcription.
- **Read-aloud starts speaking in about 2 seconds** on a typical opening
  sentence. Synthesis itself runs about 1.7x faster than real time, roughly
  585ms of compute per spoken second.
- **Speculative cleanup is not implemented.** The config key exists and the
  voice-activity detector can tell when a pause is long enough to act on, but
  nothing fires on it. Cleanup runs once, after you release the key.
- **Long reads have been seen to pause mid-passage.** The cause is unresolved.
  Machine load is the leading suspect: the same measurement on a Mac deep into
  swap came back twenty to forty times worse.
- **Spend tracking starts at zero.** Requests sent before this version were not
  tagged, so they cannot be attributed to naxvoice retroactively.

## Updating

Because this build is not signed with an Apple Developer ID, macOS ties your
permission grants to its exact signature. **Installing a new version means
granting Input Monitoring and Accessibility again.** That is a consequence of
not being notarised, not something the app chooses.

## Checksum

Verify the download before opening it:

```bash
shasum -a 256 naxvoice_0.1.0_aarch64.dmg
```

```
3c9535d6583e0cda8a4d6334687a1274b8de538ab6f7a11b78b7a04ada7ac96e
```

## Licence

naxvoice's own code is MIT.

It is not MIT all the way down, and this matters for a binary rather than for
source. The app bundles eSpeak NG's data files and links eSpeak NG itself,
which is **GPL-3.0 or later**, so this disk image carries GPL-3.0 obligations
as a whole. The Silero voice-activity model is MIT and the Kokoro weights,
voice pack and tokenizer are Apache-2.0.

The full licence texts ship inside the app, at
`naxvoice.app/Contents/Resources/resources/licenses/`.

Corresponding source for this release is the `v0.1.0` tag at
<https://github.com/nancyonyekanna/naxvoice/tree/v0.1.0>, and eSpeak NG's own
source is at <https://github.com/espeak-ng/espeak-ng>.
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) lists every component and
where it sits in the tree.
