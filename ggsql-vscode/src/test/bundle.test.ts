import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';

// out-test/test/bundle.test.js -> the extension root
const bundlePath = path.resolve(__dirname, '..', '..', 'out', 'extension.js');

suite('bundle', () => {
	test('keeps positron as an external require', () => {
		// Positron's require interceptor attributes the API object by the path
		// of the requiring file. The call has to survive bundling and stay in
		// out/extension.js, which lives inside the extension folder. If esbuild
		// inlines the module instead, every registered runtime is recorded
		// under nullExtensionDescription and session restore breaks.
		const bundle = fs.readFileSync(bundlePath, 'utf8');
		assert.match(bundle, /require\(["']positron["']\)/);
	});

	test('does not bundle the positron API helper package', () => {
		const bundle = fs.readFileSync(bundlePath, 'utf8');
		assert.doesNotMatch(bundle, /acquirePositronApi/);
	});
});
