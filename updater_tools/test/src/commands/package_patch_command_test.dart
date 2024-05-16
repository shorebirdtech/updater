import 'dart:io';

import 'package:args/args.dart';
import 'package:mason_logger/mason_logger.dart';
import 'package:mocktail/mocktail.dart';
import 'package:path/path.dart' as p;
import 'package:scoped_deps/scoped_deps.dart';
import 'package:test/test.dart';
import 'package:updater_tools/src/artifact_type.dart';
import 'package:updater_tools/src/commands/commands.dart';
import 'package:updater_tools/src/logger.dart';
import 'package:updater_tools/src/packager/patch_packager.dart';

import '../../matchers/equals_file_system_entity.dart';

class _MockArgResults extends Mock implements ArgResults {}

class _MockLogger extends Mock implements Logger {}

class _MockPatchPackager extends Mock implements PatchPackager {}

void main() {
  group(PackagePatchCommand, () {
    late ArgResults argResults;
    late Logger logger;
    late PatchPackager patchPackager;
    late PackagePatchCommand command;

    late Directory outputDirectory;
    late File releaseBundle;
    late File patchBundle;
    late File patchExecutable;

    R runWithOverrides<R>(R Function() body) {
      return runScoped(
        body,
        values: {
          loggerRef.overrideWith(() => logger),
        },
      );
    }

    setUpAll(() {
      registerFallbackValue(ArchiveType.aab);
      registerFallbackValue(Directory(''));
      registerFallbackValue(File(''));
    });

    setUp(() {
      argResults = _MockArgResults();
      logger = _MockLogger();
      patchPackager = _MockPatchPackager();

      final tempDir = Directory.systemTemp.createTempSync();
      releaseBundle = File(p.join(tempDir.path, 'release'))
        ..createSync(recursive: true);
      patchBundle = File(p.join(tempDir.path, 'patch'))
        ..createSync(recursive: true);
      patchExecutable = File(p.join(tempDir.path, 'patch.exe'))
        ..createSync(recursive: true);
      outputDirectory = Directory(p.join(tempDir.path, 'output'));

      when(() => argResults[releaseCliArg]).thenReturn(releaseBundle.path);
      when(() => argResults[patchCliArg]).thenReturn(patchBundle.path);
      when(() => argResults[patchExecutableCliArg])
          .thenReturn(patchExecutable.path);
      when(() => argResults[outputCliArg]).thenReturn(outputDirectory.path);
      when(() => argResults[archiveTypeCliArg])
          .thenReturn(ArchiveType.aab.name);

      when(
        () => patchPackager.packagePatch(
          releaseArchive: any(named: 'releaseArchive'),
          patchArchive: any(named: 'patchArchive'),
          archiveType: any(named: 'archiveType'),
          outputDirectory: any(named: 'outputDirectory'),
        ),
      ).thenAnswer(
        (invocation) async =>
            (invocation.namedArguments[#outputDirectory] as Directory)
              ..createSync(recursive: true),
      );

      command = PackagePatchCommand(
        ({required File patchExecutable}) => patchPackager,
      )..testArgResults = argResults;
    });

    group('arg validation', () {
      group('when release file does not exist', () {
        setUp(() {
          releaseBundle.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The release file does not exist')),
            ),
          );
        });
      });

      group('when patch file does not exist', () {
        setUp(() {
          patchBundle.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The patch file does not exist')),
            ),
          );
        });
      });

      group('when patch executable does not exist', () {
        setUp(() {
          patchExecutable.deleteSync();
        });

        test('logs error and exits with code 64', () async {
          expect(
            await runWithOverrides(command.run),
            equals(ExitCode.usage.code),
          );

          verify(
            () => logger.err(
              any(that: contains('The patch executable does not exist')),
            ),
          );
        });
      });
    });

    group('run', () {
      test('forwards args to patch packager', () async {
        expect(await runWithOverrides(command.run), ExitCode.success.code);

        verify(
          () => patchPackager.packagePatch(
            releaseArchive: any(
              named: 'releaseArchive',
              that: equalsFileSystemEntity(releaseBundle),
            ),
            patchArchive: any(
              named: 'patchArchive',
              that: equalsFileSystemEntity(patchBundle),
            ),
            archiveType: ArchiveType.aab,
            outputDirectory: any(
              named: 'outputDirectory',
              that: equalsFileSystemEntity(outputDirectory),
            ),
          ),
        ).called(1);
      });
    });
  });
}
