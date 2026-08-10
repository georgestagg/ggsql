import * as assert from 'assert';
import * as vscode from 'vscode';
import { GgsqlRuntimeManager, createDynState, getSupervisorApi } from '../manager';

suite('manager', () => {
	// The suites run in stock VS Code, where positron.positron-supervisor is
	// not installed. All three call sites depend on this rejecting rather than
	// resolving with a partially usable object.
	test('getSupervisorApi rejects when the supervisor is absent', async () => {
		await assert.rejects(
			() => getSupervisorApi(),
			/Positron Supervisor extension not found/,
		);
	});

	test('createDynState falls back to the default session name', () => {
		const state = createDynState();
		assert.strictEqual(state.sessionName, 'ggsql');
		assert.strictEqual(state.inputPrompt, 'ggsql> ');
		assert.strictEqual(state.continuationPrompt, '... ');
	});

	test('createDynState keeps a name Positron supplies', () => {
		// Positron passes the current session name to restoreSession. Dropping
		// it renames the console back to 'ggsql' on every window reload.
		const state = createDynState('Sales analysis');
		assert.strictEqual(state.sessionName, 'Sales analysis');
	});

	test('the manager opts out of the discovery cache fast path', () => {
		// ggsql runtimes are never marked cacheable, because the kernel path
		// can come from a workspace setting or from PATH. Without this flag
		// Positron would be free to skip discovery on a warm start and leave
		// ggsql unregistered.
		const manager = new GgsqlRuntimeManager({} as vscode.ExtensionContext);
		assert.strictEqual(manager.alwaysRediscover, true);
	});
});
