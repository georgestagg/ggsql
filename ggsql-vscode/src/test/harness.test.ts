import * as assert from 'assert';
import * as vscode from 'vscode';

suite('harness', () => {
	test('the vscode API is reachable from tests', () => {
		assert.ok(vscode.version.length > 0);
	});
});
