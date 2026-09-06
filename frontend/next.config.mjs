const nextConfig = {
  output: 'standalone',
  distDir: process.env.NEXT_DIST_DIR || '.next',
  agentRules: false,
  typescript: { ignoreBuildErrors: true },
  experimental: {
    serverActions: {
      bodySizeLimit: '10mb',
    },
  },
  turbopack: {
    resolveAlias: {
      sharp: './lib/empty-module.js',
      'onnxruntime-node': './lib/empty-module.js',
    },
  },
  webpack: (config) => {
    config.resolve.alias = {
      ...config.resolve.alias,
      sharp$: false,
      'onnxruntime-node$': false,
    };
    return config;
  },

  async headers() {
    const isolated = [
      { key: 'Cross-Origin-Opener-Policy', value: 'same-origin' },
      { key: 'Cross-Origin-Embedder-Policy', value: 'credentialless' },
    ];
    const hardened = [
      { key: 'X-Content-Type-Options', value: 'nosniff' },
      { key: 'Referrer-Policy', value: 'strict-origin-when-cross-origin' },
      { key: 'X-Frame-Options', value: 'DENY' },
      { key: 'Content-Security-Policy', value: "frame-ancestors 'none'; object-src 'none'; base-uri 'self'" },
      { key: 'Permissions-Policy', value: 'camera=(), geolocation=(), usb=(), payment=(), serial=(), hid=(), bluetooth=(), microphone=(self), display-capture=()' },
    ];
    return [
      { source: '/:path*', headers: hardened },
      { source: '/world', headers: isolated },
      { source: '/_next/static/:path*', headers: isolated },
      { source: '/docs', headers: [{ key: 'Link', value: '</docs.md>; rel="alternate"; type="text/markdown", </llms.txt>; rel="describedby"; type="text/plain"' }] },
      { source: '/docs/:slug([a-z0-9-]+)', headers: [{ key: 'Link', value: '</docs/:slug.md>; rel="alternate"; type="text/markdown", </llms.txt>; rel="describedby"; type="text/plain"' }] },
    ];
  },

  async redirects() {
    return ['about', 'privacy', 'terms', 'security'].flatMap((slug) => [
      { source: `/${slug}`, destination: `/docs/${slug}`, permanent: true },
      { source: `/${slug}.md`, destination: `/docs/${slug}.md`, permanent: true },
    ]).concat([
      { source: '/privacy-policy', destination: '/docs/privacy', permanent: true },
      { source: '/terms-of-service', destination: '/docs/terms', permanent: true },
      { source: '/tos', destination: '/docs/terms', permanent: true },
    ]);
  },

  async rewrites() {
    const apiUrl = process.env.API_URL || 'http://localhost:8081';
    return [
      {
        source: '/api/:path*',
        destination: `${apiUrl}/api/:path*`,
      },
      {
        source: '/mcp',
        destination: `${apiUrl}/mcp`,
      },
      {
        source: '/webhooks/:provider(linear|clickup)',
        destination: `${apiUrl}/webhooks/:provider`,
      },
    ];
  },
};

export default nextConfig;
