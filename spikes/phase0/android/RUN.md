# 0.9a: run the Android spike on your phone

No phone or emulator was available to the agent, so the on-device numbers come from you.
Needs: an Android phone with a **64-bit ARM** CPU (almost every phone since 2017), Android 8.0 or newer.

## 1. Get the APKs

Three APKs, one per Wasm runtime. They install side by side (different app IDs):

| File | Runtime |
|---|---|
| `app-pulley-release.apk` | Wasmtime + Pulley interpreter (the candidate for v1) |
| `app-wasmi-release.apk` | wasmi interpreter (alternative) |
| `app-cranelift-release.apk` | Wasmtime + Cranelift JIT (for comparison only) |

They are built into `spikes/phase0/android/app-spike/app/build/outputs/apk/<flavor>/release/`. To rebuild:

```bash
# Git Bash, from the repo root
spikes/phase0/android/build-so.sh                       # builds the three .so files
cd spikes/phase0/android/app-spike && ./gradlew assembleRelease
```

The APKs are signed with a local **debug key**. They are test builds: do not share them.

## 2. Install

**Option A: USB + adb (best, it also gives the log)**
1. On the phone: Settings > About phone > tap "Build number" 7 times. Then Settings > System > Developer options >
   turn on "USB debugging".
2. Connect the USB cable and accept the "Allow USB debugging?" prompt on the phone.
3. On the PC:
   ```
   D:\Android\Sdk\platform-tools\adb.exe devices
   D:\Android\Sdk\platform-tools\adb.exe install -r app-pulley-release.apk
   D:\Android\Sdk\platform-tools\adb.exe install -r app-wasmi-release.apk
   D:\Android\Sdk\platform-tools\adb.exe install -r app-cranelift-release.apk
   ```

**Option B: file transfer**
Copy the APKs to the phone (USB file transfer, Google Drive, and so on), open them in the Files app and allow
"Install unknown apps" for that app when asked. Uninstall the spike apps afterwards.

## 3. Test (do this for each of the three apps; each is called "NC Wasm spike")

1. Open the app. Allow notifications when asked.
2. **Run to the end:** tap **Start test job**. A "Helping" notification with a **Stop** button appears.
   Wait until the screen shows a line starting with `RESULT ... sum=...`. Write the line down.
3. **Stop from the app:** tap **Start test job**, wait about 2 seconds, tap **Stop**.
   Write down the `RESULT ... stopped stop_latency_ms=...` line.
4. **Stop from the notification:** tap **Start test job**, go to the home screen, pull down the notification bar,
   tap **Stop** on the "Helping" notification. Open the app again and write down the `RESULT` line.
5. **Screen off (optional, about 5 minutes):** start the job, then lock the phone straight away. Unlock it after
   5 minutes and write down whether a `RESULT ... sum=` line appeared, or whether the app was closed.

If you used adb you can also capture everything with:
```
D:\Android\Sdk\platform-tools\adb.exe logcat -s NcSpike:I AndroidRuntime:E
```

## 4. What to send back

- Phone make, model, Android version (Settings > About phone).
- For each of the three apps: the `RESULT` lines from steps 2, 3, 4 (and 5 if done).
- Anything odd: crash, the app or notification disappearing, a warning from Play Protect.

The expected result for step 2 is `sum=44999999850000000` (sum of 0 to 299,999,999).
