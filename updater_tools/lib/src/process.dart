import 'package:process/process.dart';
import 'package:scoped_deps/scoped_deps.dart';

/// A reference to a [ProcessManager] instance.
final ScopedRef<ProcessManager> processManagerRef =
    create(LocalProcessManager.new);

/// The [ProcessManager] instance available in the current zone.
ProcessManager get processManager => read(processManagerRef);
