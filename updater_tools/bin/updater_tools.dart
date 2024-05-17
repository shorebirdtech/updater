import 'dart:io';

import 'package:scoped_deps/scoped_deps.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/process.dart';
import 'package:updater_tools/src/updater_tools_command_runner.dart';

Future<void> main(List<String> args) async {
  await _flushThenExit(
    await runScoped(
      () async => UpdaterToolsCommandRunner().run(args),
      values: {
        loggerRef,
        processManagerRef,
      },
    ),
  );
}

/// Flushes the stdout and stderr streams, then exits the program with the given
/// status code.
///
/// This returns a Future that will never complete, since the program will have
/// exited already. This is useful to prevent Future chains from proceeding
/// after you've decided to exit.
Future<void> _flushThenExit(int status) {
  return Future.wait<void>([stdout.close(), stderr.close()])
      .then<void>((_) => exit(status));
}
