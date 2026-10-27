
info-fail = Failed to load info.
invalid-chart = Invalid chart.

importing = Importing...
import-success = Imported successfully.
import-failed = Import failed.
import-respack-success = Imported successfully.
import-respack-failed = Failed to import respack.

batch-import = Batch Import
batch-importing = Batch importing... ({ $current }/{ $total })
batch-import-confirm = Batch import data detected. Import { $count } charts?
batch-import-success = Successfully imported { $count } charts
batch-import-downloaded-skipped = Charts skipped due to already downloaded: { $charts }
batch-import-failed = Batch import failed
batch-import-failed-chart = Batch import failed: { $chart }

warning = Warning
warning-new-speed-event = This chart uses the speed event easing introduced in RPE 1.7.0. For compatibility reasons, Phira does not enable support for this event by default. If you want to enable it, please check "New Speed Tween" in the chart information.
warning-attach-ui = This chart uses UI attachment. Recent versions of Phira introduced a fix for UI attachment that may cause issues with charts that rely on the old behavior. If you encounter any problems, please uncheck "Attach UI Fix" in the chart information.

warning-rpen-features = This chart uses RPEN extended features (curve trajectory, tangent rotation, judge-line BPM factor, skewControl, layered noise zones, shader events, globalEvents, ...). These are in early testing and may render or judge incorrectly. If you run into problems, turn off "RPEN Features" in the chart information to fall back to the legacy behavior.
