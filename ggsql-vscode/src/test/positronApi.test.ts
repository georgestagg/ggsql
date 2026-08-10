import * as assert from 'assert';
import { getPositronApi } from '../positronApi';

suite('positronApi', () => {
	// The suites run in stock VS Code, where the 'positron' module does not
	// exist. Activation calls this on every host, so it must not throw here.
	test('returns undefined outside Positron instead of throwing', () => {
		assert.strictEqual(getPositronApi(), undefined);
	});
});
