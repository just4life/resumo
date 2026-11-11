package org.resumo.app;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

/**
 * Keeps Resumo playing while the screen is off or another app is in front:
 * a foreground service with the media notification, holding the CPU (and
 * Wi-Fi, for network videos) awake while something plays.
 */
public final class PlaybackService extends Service {
    private PowerManager.WakeLock wake;
    private WifiManager.WifiLock wifi;

    static PendingIntent intent(Context ctx, int code) {
        Intent i = new Intent(ctx, PlaybackService.class).setAction("resumo." + code).putExtra("code", code);
        return PendingIntent.getService(ctx, code, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    /** Re-posts the notification without (re)starting the service. */
    static void refresh(Context ctx) {
        try {
            ((NotificationManager) ctx.getSystemService(Context.NOTIFICATION_SERVICE))
                    .notify(Playback.NOTIFICATION_ID, Playback.notification(ctx));
        } catch (RuntimeException ignored) {
        }
    }

    @Override public void onCreate() {
        super.onCreate();
        wake = ((PowerManager) getSystemService(POWER_SERVICE)).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "resumo:playback");
        wake.setReferenceCounted(false);
        wifi = ((WifiManager) getApplicationContext().getSystemService(WIFI_SERVICE))
                .createWifiLock(Build.VERSION.SDK_INT >= 29 ? WifiManager.WIFI_MODE_FULL_HIGH_PERF : WifiManager.WIFI_MODE_FULL, "resumo:playback");
        wifi.setReferenceCounted(false);
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        Notification n = Playback.notification(this);
        try {
            if (Build.VERSION.SDK_INT >= 29) startForeground(Playback.NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            else startForeground(Playback.NOTIFICATION_ID, n);
        } catch (RuntimeException e) {
            refresh(this);
        }
        int code = intent != null ? intent.getIntExtra("code", 0) : 0;
        if (code != 0) Playback.action(code, 0);
        if (Playback.isPlaying()) {
            wake.acquire();
            wifi.acquire();
        } else {
            if (wake.isHeld()) wake.release();
            if (wifi.isHeld()) wifi.release();
            // Paused: the notification may be swiped away, the service goes
            // on standing by until playback ends.
            if (Build.VERSION.SDK_INT >= 24) stopForeground(STOP_FOREGROUND_DETACH);
        }
        return START_NOT_STICKY;
    }

    @Override public void onDestroy() {
        if (wake.isHeld()) wake.release();
        if (wifi.isHeld()) wifi.release();
        ((NotificationManager) getSystemService(NOTIFICATION_SERVICE)).cancel(Playback.NOTIFICATION_ID);
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) {
        return null;
    }
}
