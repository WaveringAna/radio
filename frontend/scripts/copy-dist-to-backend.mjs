import { cp, rm } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const frontendRoot = resolve(scriptDir, '..')
const workspaceRoot = resolve(frontendRoot, '..')
const destination = resolve(workspaceRoot, 'static')

if (process.env.VITE_STANDALONE === 'true') {
  console.log('standalone build detected: skipping copying files to backend static/ directory.')
  process.exit(0)
}

// The bundle served by the Rust backend at /static must use relative URLs so it
// works on any host. If the outer `vite build` baked in a VITE_API_BASE (e.g.
// for an external deploy at https://radio.nekomimi.pet), redo the build here
// with the env var cleared into a sibling output dir, and copy that one.
const apiBase = process.env.VITE_API_BASE ?? ''
let source = resolve(frontendRoot, 'dist')

if (apiBase) {
  source = resolve(frontendRoot, 'dist-static')
  await rm(source, { force: true, recursive: true })
  const env = { ...process.env }
  delete env.VITE_API_BASE
  const result = spawnSync(
    'npx',
    ['vite', 'build', '--outDir', source, '--emptyOutDir'],
    { cwd: frontendRoot, stdio: 'inherit', env },
  )
  if (result.status !== 0) {
    process.exit(result.status ?? 1)
  }
}

await rm(destination, { force: true, recursive: true })
await cp(source, destination, { recursive: true })

if (apiBase) {
  await rm(source, { force: true, recursive: true })
  console.log(`copied relative-URL build to ${destination} (dist/ retains VITE_API_BASE=${apiBase})`)
} else {
  console.log(`copied ${source} to ${destination}`)
}
