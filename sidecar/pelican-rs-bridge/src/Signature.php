<?php

namespace PelicanRs\Bridge;

use Illuminate\Http\Request;

/**
 * Requests from pelican-rs carry `X-Pelican-Rs-Timestamp` and `X-Pelican-Rs-Signature`
 * = hex HMAC-SHA256 over "<METHOD>\n<path>\n<timestamp>\n<user id or empty>\n<sha256(body)>"
 * with the key HMAC-SHA256("pelican-rs-sidecar", APP_KEY). Proxied page requests sign
 * method "PROXY" and path "/" with an empty body: Livewire re-runs the page's middleware
 * on a request rebuilt from the original page, so their method and path are not stable.
 */
final class Signature
{
    public const MAX_SKEW = 60;

    public static function secret(): string
    {
        return hash_hmac('sha256', 'pelican-rs-sidecar', (string) config('app.key'), true);
    }

    public static function validProxy(Request $request): bool
    {
        return self::check($request, 'PROXY', '/', '');
    }

    public static function valid(Request $request): bool
    {
        return self::check($request, strtoupper($request->getMethod()), '/' . ltrim($request->path(), '/'), $request->getContent());
    }

    private static function check(Request $request, string $method, string $path, string $body): bool
    {
        $timestamp = (string) $request->header('X-Pelican-Rs-Timestamp', '');
        $signature = (string) $request->header('X-Pelican-Rs-Signature', '');
        if (!ctype_digit($timestamp) || abs(time() - (int) $timestamp) > self::MAX_SKEW || $signature === '') {
            return false;
        }

        $message = implode("\n", [
            $method,
            $path,
            $timestamp,
            (string) $request->header('X-Pelican-Rs-User', ''),
            hash('sha256', $body),
        ]);

        return hash_equals(hash_hmac('sha256', $message, self::secret()), $signature);
    }
}
