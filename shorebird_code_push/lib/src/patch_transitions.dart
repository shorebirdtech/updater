import 'dart:async';

import 'package:shorebird_code_push/src/shorebird_updater.dart';

/// Where [ShorebirdUpdater] publishes a patch transition and
/// `ShorebirdHotRestart` reads it.
///
/// One sink for the process rather than one per object. A watcher the app
/// built separately has to hear about an `update()` the app ran through the
/// updater, and two surfaces reading the same disk must not be able to
/// disagree about what it says.
///
/// Broadcast, so it buffers nothing: a transition nobody was listening for is
/// gone, and a late listener recovers it by reading the patches itself.
final patchTransitionPublisher = StreamController<PatchTransition>.broadcast();
