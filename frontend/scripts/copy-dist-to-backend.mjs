import { cp, rm } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const frontendRoot = resolve(scriptDir, '..')
const workspaceRoot = resolve(frontendRoot, '..')
const source = resolve(frontendRoot, 'dist')
const destination = resolve(workspaceRoot, 'static')

await rm(destination, { force: true, recursive: true })
await cp(source, destination, { recursive: true })

console.log(`copied ${source} to ${destination}`)
