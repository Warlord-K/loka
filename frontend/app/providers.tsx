'use client';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import dynamic from 'next/dynamic';
import { TooltipLayer } from '@/components/tooltip-layer';
import { UpdateCard } from '@/components/world/update-card';

export function Providers({ children }: { children: React.ReactNode }) {
  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 30_000,
            gcTime: 10 * 60_000,
            refetchOnWindowFocus: true,
            retry: 1,
          },
        },
      })
  );

  return (
    <QueryClientProvider client={queryClient}>
      {children}
      <TooltipLayer />
      <UpdateCard />
    </QueryClientProvider>
  );
}
