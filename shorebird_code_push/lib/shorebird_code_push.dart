/// Get info about your Shorebird code push app
library shorebird_code_push;

export 'src/shorebird_code_push_io.dart'
    if (dart.library.js_interop) 'src/shorebird_code_push_web.dart';
export 'src/shorebird_updater.dart';
