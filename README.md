# reiss-mcpherson-effects

Audio plugins from Reiss & McPherson's *Audio Effects: Theory,
Implementation and Application*, ported from
[Juan Gil's JUCE implementations](https://github.com/juandagilc/Audio-Effects)
to the [truce](https://github.com/truce-audio/truce) framework.

Each plugin builds as CLAP / VST3 / LV2 / AU / AAX / Standalone.

## Plugins

Screenshots below are rendered with `cargo truce screenshot` at
the default 2× scale, so every PNG is captured at the same
fixed DPI; the `width` / `height` on each `<img>` are the
plugin's logical-point dimensions, which keeps the rendered
density identical regardless of widget count.

| Crate                                      | Effect                                | Screenshot |
| ------------------------------------------ | ------------------------------------- | ---------- |
| `reiss-mcpherson-delay`                    | Circular-buffer delay                 | <img src="screenshots/reiss-mcpherson-delay.png" width="208" height="113" alt="Reiss Delay editor"> |
| `reiss-mcpherson-vibrato`                  | LFO-modulated delay (pitch wobble)    | <img src="screenshots/reiss-mcpherson-vibrato.png" width="415" height="113" alt="Reiss Vibrato editor"> |
| `reiss-mcpherson-flanger`                  | Modulated short delay + dry sum       | <img src="screenshots/reiss-mcpherson-flanger.png" width="484" height="182" alt="Reiss Flanger editor"> |
| `reiss-mcpherson-chorus`                   | Multi-voice ensemble chorus           | <img src="screenshots/reiss-mcpherson-chorus.png" width="346" height="182" alt="Reiss Chorus editor"> |
| `reiss-mcpherson-pingpong`                 | Cross-channel ping-pong delay         | <img src="screenshots/reiss-mcpherson-pingpong.png" width="277" height="113" alt="Reiss Ping-Pong editor"> |
| `reiss-mcpherson-parametric-eq`            | Single-band parametric EQ (7 shapes)  | <img src="screenshots/reiss-mcpherson-parametric-eq.png" width="346" height="113" alt="Reiss Parametric EQ editor"> |
| `reiss-mcpherson-wahwah`                   | Manual / LFO / envelope wah           | <img src="screenshots/reiss-mcpherson-wahwah.png" width="415" height="210" alt="Reiss Wah-Wah editor"> |
| `reiss-mcpherson-phaser`                   | Cascaded all-pass phaser              | <img src="screenshots/reiss-mcpherson-phaser.png" width="346" height="182" alt="Reiss Phaser editor"> |
| `reiss-mcpherson-tremolo`                  | LFO amplitude modulation              | <img src="screenshots/reiss-mcpherson-tremolo.png" width="277" height="113" alt="Reiss Tremolo editor"> |
| `reiss-mcpherson-ringmod`                  | Ring modulation                       | <img src="screenshots/reiss-mcpherson-ringmod.png" width="277" height="113" alt="Reiss Ring Mod editor"> |
| `reiss-mcpherson-compressor`               | Compressor / expander / gate          | <img src="screenshots/reiss-mcpherson-compressor.png" width="277" height="182" alt="Reiss Compressor editor"> |
| `reiss-mcpherson-distortion`               | 5-shape waveshaper + tone shelf       | <img src="screenshots/reiss-mcpherson-distortion.png" width="346" height="113" alt="Reiss Distortion editor"> |
| `reiss-mcpherson-panning`                  | Panorama+precedence / ITD+ILD pan     | <img src="screenshots/reiss-mcpherson-panning.png" width="208" height="113" alt="Reiss Panning editor"> |
| `reiss-mcpherson-robotization`             | Phase-vocoder robot / whisper         | <img src="screenshots/reiss-mcpherson-robotization.png" width="415" height="113" alt="Reiss Robotization editor"> |
| `reiss-mcpherson-pitchshift`               | Phase-vocoder pitch shifter           | <img src="screenshots/reiss-mcpherson-pitchshift.png" width="277" height="113" alt="Reiss Pitch Shift editor"> |

## Build

See the [truce repo](https://github.com/truce-audio/truce) for
build, install and packaging instructions.

## Licensing

Dual-licensed under Apache-2.0 OR MIT, at your option.
