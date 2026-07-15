import fs from 'fs';
import path from 'path';

const baseUrl = (process.env.VITE_BASE_URL || '').trim().replace(/\/+$/, '');
const standalone = process.env.VITE_STANDALONE === 'true';

if (standalone && baseUrl) {
  const metadata = {
    "client_id": `${baseUrl}/client-metadata.json`,
    "client_name": "radio",
    "client_uri": baseUrl,
    "application_type": "native",
    "dpop_bound_access_tokens": true,
    "grant_types": [
      "authorization_code",
      "refresh_token"
    ],
    "redirect_uris": [
      `${baseUrl}/auth`
    ],
    "response_types": [
      "code"
    ],
    "scope": "atproto rpc?aud=*&lxm=pet.nkp.radio.admin.modify&lxm=pet.nkp.radio.admin.permissions&lxm=pet.nkp.radio.albums.list&lxm=pet.nkp.radio.albums.modify&lxm=pet.nkp.radio.chat.bans.list&lxm=pet.nkp.radio.chat.bans.modify&lxm=pet.nkp.radio.chat.messages.modify&lxm=pet.nkp.radio.chat.send&lxm=pet.nkp.radio.control&lxm=pet.nkp.radio.playlists.list&lxm=pet.nkp.radio.playlists.modify&lxm=pet.nkp.radio.queue.list&lxm=pet.nkp.radio.queue.modify&lxm=pet.nkp.radio.songs.add&lxm=pet.nkp.radio.songs.cover&lxm=pet.nkp.radio.songs.list&lxm=pet.nkp.radio.songs.modify&lxm=pet.nkp.radio.songs.upload&lxm=pet.nkp.radio.subsonic.import&lxm=pet.nkp.radio.subsonic.search",
    "token_endpoint_auth_method": "none"
  };

  const publicDir = path.resolve('public');
  if (!fs.existsSync(publicDir)) {
    fs.mkdirSync(publicDir, { recursive: true });
  }
  
  fs.writeFileSync(
    path.join(publicDir, 'client-metadata.json'),
    JSON.stringify(metadata, null, 2)
  );
  console.log(`Successfully generated client-metadata.json for standalone url: ${baseUrl}`);
} else {
  const filePath = path.resolve('public/client-metadata.json');
  if (fs.existsSync(filePath)) {
    fs.unlinkSync(filePath);
    console.log('Removed client-metadata.json from public directory (non-standalone build)');
  }
}
