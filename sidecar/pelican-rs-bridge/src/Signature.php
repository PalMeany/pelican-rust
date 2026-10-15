<?php

namespace PelicanRs\Bridge;

use Illuminate\Http\Request;

/**
 * Requests from pelican-rs carry `X-Pelican-Rs-Timestamp` and `X-Pelican-Rs-Signature`
 * = hex HMAC-SHA256 over "<METHOD>\n<path>\n<timestamp>\n<user id or empty>\n<sha256(body)>"
 * with the key HMAC-SHA256("pelican-rs-sidecar", APP_KEY).
 */
final class Signature
{
    public const MAX_SKEW = 60;

    public static function secret(): string
    {
        return hash_hmac('sha256', 'pelican-rs-sidecar', (string) config('app.key'), true);
    }

    public static function valid(Request $request, bool $withBody): bool
    {
        $timestamp = (string) $request->header('X-Pelican-Rs-Timestamp', '');
        $signature = (string) $request->header('X-Pelican-Rs-Signature', '');
        if (!ctype_digit($timestamp) || abs(time() - (int) $timestamp) > self::MAX_SKEW || $signature === '') {
            return false;
        }

        $message = implode("\n", [
            strtoupper($request->getMethod()),
            '/' . ltrim($request->path(), '/'),
            $timestamp,
            (string) $request->header('X-Pelican-Rs-User', ''),
            hash('sha256', $withBody ? $request->getContent() : ''),
        ]);

        return hash_equals(hash_hmac('sha256', $message, self::secret()), $signature);
    }
}
