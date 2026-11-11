// One raw POST with full control over every header.
//   node secret-domain-post.cjs <port> <headers-json> <body>
// Prints the response status code, or ERR on transport error.
const http = require('node:http');
const [port, headersJson, body] = process.argv.slice(2);
const headers = JSON.parse(headersJson);
const req = http.request(
  { host: '127.0.0.1', port: Number(port), path: '/hook', method: 'POST', headers },
  (res) => { res.resume(); res.on('end', () => console.log(res.statusCode)); },
);
req.on('error', () => console.log('ERR'));
req.end(body);
